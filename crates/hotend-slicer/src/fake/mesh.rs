//! Volume and bounding box of an STL or 3MF mesh, for the fake slicer's estimates.
//!
//! This is not a general mesh reader: it handles binary and ASCII STL and the plain
//! `<vertex>` / `<triangle>` lists of a 3MF model part, ignores transforms, components and
//! units, and counts each mesh once. That is enough to make the fake slicer's numbers move
//! with the size of the model.

use std::io::Read;
use std::path::Path;

/// Largest mesh file, and largest 3MF model part, the fake slicer reads.
const MAX_MESH_BYTES: u64 = 256 * 1024 * 1024;

/// What the fake slicer needs to know about a mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshStats {
    pub volume_mm3: f64,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub triangles: u64,
}

impl Default for MeshStats {
    fn default() -> Self {
        Self {
            volume_mm3: 0.0,
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
            triangles: 0,
        }
    }
}

impl MeshStats {
    fn add(&mut self, a: [f64; 3], b: [f64; 3], c: [f64; 3]) {
        // Signed volume of the tetrahedron between the triangle and the origin.
        let cross = [
            b[1] * c[2] - b[2] * c[1],
            b[2] * c[0] - b[0] * c[2],
            b[0] * c[1] - b[1] * c[0],
        ];
        self.volume_mm3 += (a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2]) / 6.0;
        for point in [a, b, c] {
            for (axis, value) in point.into_iter().enumerate() {
                self.min[axis] = self.min[axis].min(value);
                self.max[axis] = self.max[axis].max(value);
            }
        }
        self.triangles += 1;
    }

    /// Height of the mesh along Z.
    pub fn height(&self) -> f64 {
        self.max[2] - self.min[2]
    }

    fn finish(mut self) -> Result<Self, String> {
        self.volume_mm3 = self.volume_mm3.abs();
        let finite =
            self.volume_mm3.is_finite() && self.min.iter().chain(&self.max).all(|v| v.is_finite());
        if self.triangles == 0 || !finite {
            return Err("the mesh has no usable triangles".into());
        }
        Ok(self)
    }
}

/// Reads the mesh in `path`, an `.stl` or `.3mf` file.
pub fn read(path: &Path) -> Result<MeshStats, String> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > MAX_MESH_BYTES {
        return Err(format!(
            "{} is too large for the fake slicer",
            path.display()
        ));
    }
    match extension.as_str() {
        "stl" => {
            let mut bytes = Vec::new();
            file.take(MAX_MESH_BYTES)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            stl(&bytes)
        }
        "3mf" => threemf(file),
        other => Err(format!(
            "the fake slicer reads STL and 3MF meshes only, not `.{other}` files"
        )),
    }
}

fn stl(bytes: &[u8]) -> Result<MeshStats, String> {
    let mut stats = MeshStats::default();
    let binary_count = bytes
        .get(80..84)
        .map(|count| u32::from_le_bytes([count[0], count[1], count[2], count[3]]) as usize);
    if let Some(count) = binary_count
        && count.checked_mul(50).and_then(|n| n.checked_add(84)) == Some(bytes.len())
    {
        for triangle in bytes[84..].chunks_exact(50) {
            let vertex = |index: usize| {
                let at = 12 + index * 12;
                let value = |offset: usize| {
                    let b = &triangle[at + offset..at + offset + 4];
                    f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                };
                [value(0), value(4), value(8)]
            };
            stats.add(vertex(0), vertex(1), vertex(2));
        }
        return stats.finish();
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "the STL file is neither binary nor text")?;
    let mut corners: Vec<[f64; 3]> = Vec::with_capacity(3);
    for line in text.lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("vertex") {
            continue;
        }
        let mut point = [0.0; 3];
        for value in &mut point {
            *value = words
                .next()
                .and_then(|word| word.parse().ok())
                .ok_or("a vertex line in the STL file is malformed")?;
        }
        corners.push(point);
        if corners.len() == 3 {
            stats.add(corners[0], corners[1], corners[2]);
            corners.clear();
        }
    }
    stats.finish()
}

/// The `inner` elements inside the `outer` elements of `node`.
fn grandchildren<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    outer: &str,
    inner: &str,
) -> Vec<roxmltree::Node<'a, 'input>> {
    let named = |node: &roxmltree::Node<'_, '_>, name: &str| {
        node.is_element() && node.tag_name().name() == name
    };
    node.children()
        .filter(|child| named(child, outer))
        .flat_map(|child| child.children())
        .filter(|child| named(child, inner))
        .collect()
}

fn threemf(file: std::fs::File) -> Result<MeshStats, String> {
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a 3MF archive: {e}"))?;
    let name = (0..archive.len().min(4096))
        .filter_map(|index| {
            archive
                .name_for_index(index)?
                .ok()
                .map(|name| name.into_owned())
        })
        .find(|name| name.to_ascii_lowercase().ends_with(".model"))
        .ok_or("the 3MF archive has no model part")?;
    let mut text = String::new();
    archive
        .by_name(&name)
        .map_err(|e| e.to_string())?
        .take(MAX_MESH_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| format!("the model part cannot be read: {e}"))?;
    let document =
        roxmltree::Document::parse(&text).map_err(|e| format!("the model part is not XML: {e}"))?;
    let mut stats = MeshStats::default();
    for mesh in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "mesh")
    {
        let mut vertices: Vec<[f64; 3]> = Vec::new();
        for vertex in grandchildren(mesh, "vertices", "vertex") {
            let coordinate = |name: &str| {
                vertex
                    .attribute(name)
                    .and_then(|value| value.parse::<f64>().ok())
                    .ok_or("a vertex in the 3MF model is malformed")
            };
            vertices.push([coordinate("x")?, coordinate("y")?, coordinate("z")?]);
        }
        for triangle in grandchildren(mesh, "triangles", "triangle") {
            let corner = |name: &str| {
                triangle
                    .attribute(name)
                    .and_then(|value| value.parse::<usize>().ok())
                    .and_then(|index| vertices.get(index).copied())
                    .ok_or("a triangle in the 3MF model is malformed")
            };
            stats.add(corner("v1")?, corner("v2")?, corner("v3")?);
        }
    }
    stats.finish()
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use std::io::Write;

    use super::*;

    /// The twelve triangles of an axis-aligned box from the origin.
    pub(crate) fn box_triangles(size: [f64; 3]) -> Vec<[[f64; 3]; 3]> {
        let [x, y, z] = size;
        let p = |i: usize| {
            [
                if i & 1 == 0 { 0.0 } else { x },
                if i & 2 == 0 { 0.0 } else { y },
                if i & 4 == 0 { 0.0 } else { z },
            ]
        };
        [
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ]
        .into_iter()
        .map(|[a, b, c]| [p(a), p(b), p(c)])
        .collect()
    }

    pub(crate) fn ascii_stl(size: [f64; 3]) -> String {
        let mut text = String::from("solid box\n");
        for triangle in box_triangles(size) {
            text.push_str(" facet normal 0 0 0\n  outer loop\n");
            for [x, y, z] in triangle {
                text.push_str(&format!("   vertex {x} {y} {z}\n"));
            }
            text.push_str("  endloop\n endfacet\n");
        }
        text.push_str("endsolid box\n");
        text
    }

    pub(crate) fn binary_stl(size: [f64; 3]) -> Vec<u8> {
        let triangles = box_triangles(size);
        let mut bytes = vec![0u8; 80];
        bytes.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
        for triangle in triangles {
            bytes.extend_from_slice(&[0u8; 12]);
            for point in triangle {
                for value in point {
                    bytes.extend_from_slice(&(value as f32).to_le_bytes());
                }
            }
            bytes.extend_from_slice(&[0u8; 2]);
        }
        bytes
    }

    pub(crate) fn threemf_bytes(size: [f64; 3]) -> Vec<u8> {
        let [x, y, z] = size;
        let mut model = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\"><resources><object id=\"1\" type=\"model\"><mesh><vertices>",
        );
        for i in 0..8 {
            let c = |bit: usize, v: f64| if i & bit == 0 { 0.0 } else { v };
            model.push_str(&format!(
                "<vertex x=\"{}\" y=\"{}\" z=\"{}\"/>",
                c(1, x),
                c(2, y),
                c(4, z)
            ));
        }
        model.push_str("</vertices><triangles>");
        for [a, b, c] in [
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ] {
            model.push_str(&format!("<triangle v1=\"{a}\" v2=\"{b}\" v3=\"{c}\"/>"));
        }
        model.push_str(
            "</triangles></mesh></object></resources><build><item objectid=\"1\"/></build></model>",
        );
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("[Content_Types].xml", options).unwrap();
        writer.write_all(b"<Types/>").unwrap();
        writer.start_file("3D/3dmodel.model", options).unwrap();
        writer.write_all(model.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn a_box_has_its_volume_and_height_in_every_format() {
        let dir = tempfile::tempdir().unwrap();
        let size = [20.0, 30.0, 10.0];
        std::fs::write(dir.path().join("a.stl"), ascii_stl(size)).unwrap();
        std::fs::write(dir.path().join("b.STL"), binary_stl(size)).unwrap();
        std::fs::write(dir.path().join("c.3mf"), threemf_bytes(size)).unwrap();
        for name in ["a.stl", "b.STL", "c.3mf"] {
            let stats = read(&dir.path().join(name)).unwrap();
            assert!(
                (stats.volume_mm3 - 6000.0).abs() < 1e-6,
                "{name}: {stats:?}"
            );
            assert_eq!(stats.height(), 10.0, "{name}");
            assert_eq!(stats.triangles, 12, "{name}");
            assert_eq!((stats.min, stats.max), ([0.0; 3], size), "{name}");
        }
    }

    #[test]
    fn unusable_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        };
        assert!(read(&write("empty.stl", b"")).is_err());
        assert!(read(&write("text.stl", b"solid x\nendsolid x\n")).is_err());
        assert!(read(&write("bad.stl", b"vertex 1 2 oops\n")).is_err());
        assert!(
            read(&write("model.step", b"ISO-10303-21;"))
                .unwrap_err()
                .contains("STL and 3MF")
        );
        assert!(read(&write("not-a-zip.3mf", b"hello")).is_err());
        assert!(read(&dir.path().join("missing.stl")).is_err());
    }
}
