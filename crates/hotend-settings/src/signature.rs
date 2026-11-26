//! The fit signature: the print settings a fit gauge was made with, and canonical hashing.
//!
//! A gauge export stores the signature, taken from the values the sliced file states. A
//! product for the same printer, material and nozzle is then printed with the same first
//! layer, and warned about when it is not.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::bounds::{self, RangeError};
use crate::model::{Field, MaterialId, PrintPlan, ProcessPreset, Value};

/// The values of a fit signature.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignatureParts {
    /// The process preset.
    pub process: ProcessPreset,
    /// The layer height in mm.
    pub layer_height_mm: f64,
    /// The first layer height in mm.
    pub first_layer_height_mm: f64,
    /// The first layer line width in mm.
    pub first_layer_line_width_mm: f64,
    /// The elephant foot compensation in mm.
    pub elephant_foot_mm: f64,
    /// The outer wall speed cap in mm/s, when there was one.
    pub outer_wall_cap_mm_s: Option<i64>,
    /// The bed temperature of the first layer.
    pub first_layer_bed_c: i64,
    /// The bed temperature of the other layers.
    pub layers_bed_c: i64,
    /// The gap between brim and part in mm.
    pub brim_gap_mm: f64,
    /// The material.
    pub material_id: MaterialId,
    /// The device extruder id of the nozzle.
    pub nozzle_id: u8,
}

/// A checked fit signature: every value lies inside the hard range of its field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FitSignature(SignatureParts);

impl FitSignature {
    /// Checks the values against the hard ranges.
    pub fn new(parts: SignatureParts) -> Result<Self, RangeError> {
        let layer = parts.layer_height_mm;
        if !layer.is_finite() {
            return Err(RangeError::NotFinite(Field::Process));
        }
        if !(0.08..=bounds::FIRST_LAYER_HEIGHT_MM.1).contains(&layer) {
            return Err(RangeError::OutOfRange {
                field: Field::Process,
                min: 0.08,
                max: bounds::FIRST_LAYER_HEIGHT_MM.1,
            });
        }
        bounds::check(
            Field::FirstLayerHeight,
            &Value::Decimal(parts.first_layer_height_mm),
        )?;
        bounds::check(
            Field::FirstLayerLineWidth,
            &Value::Decimal(parts.first_layer_line_width_mm),
        )?;
        bounds::check(Field::ElephantFoot, &Value::Decimal(parts.elephant_foot_mm))?;
        if let Some(cap) = parts.outer_wall_cap_mm_s {
            bounds::check(Field::OuterWallSpeedCap, &Value::Whole(cap))?;
        }
        bounds::check(Field::FirstLayerBed, &Value::Whole(parts.first_layer_bed_c))?;
        bounds::check(Field::LayersBed, &Value::Whole(parts.layers_bed_c))?;
        bounds::check(Field::BrimGap, &Value::Decimal(parts.brim_gap_mm))?;
        bounds::check(Field::NozzleId, &Value::Whole(i64::from(parts.nozzle_id)))?;
        Ok(Self(parts))
    }

    /// The values.
    pub fn parts(&self) -> &SignatureParts {
        &self.0
    }

    /// The SHA-256 of the signature's canonical JSON, in lower-case hexadecimal.
    pub fn sha256(&self) -> Result<String, serde_json::Error> {
        Ok(sha256_hex(canonical_json(&self.0)?.as_bytes()))
    }
}

impl PrintPlan {
    /// The SHA-256 of the plan's canonical JSON. Equal inputs and equal data files give an
    /// equal hash; the core compares it when it recomputes a stored plan.
    pub fn sha256(&self) -> Result<String, serde_json::Error> {
        Ok(sha256_hex(canonical_json(self)?.as_bytes()))
    }
}

/// JSON with the keys of every object sorted and no white space, so that equal values give
/// equal bytes whatever order a map was built in.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    write_canonical(&serde_json::to_value(value)?, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &serde_json::Value, out: &mut String) -> Result<(), serde_json::Error> {
    match value {
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                write_canonical(&map[key.as_str()], out)?;
            }
            out.push('}');
        }
        scalar => out.push_str(&serde_json::to_string(scalar)?),
    }
    Ok(())
}

/// SHA-256 in lower-case hexadecimal.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
