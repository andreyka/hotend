// The token stylesheet is the block of the design specification, section 7.1, and the
// vendored components hold no colour of their own.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(join(import.meta.dirname, path), "utf8");

/** The text of the first ```css block after the heading of section 7.1. */
function specificationBlock(): string {
  const spec = read("../../../docs/ux/DESIGN_SPEC.md");
  const section = spec.slice(spec.indexOf("### 7.1 Tokens"));
  const start = section.indexOf("```css\n") + "```css\n".length;
  return section.slice(start, section.indexOf("\n```", start));
}

/** The stylesheet without its leading comment. */
function stylesheet(): string {
  const css = read("../src/styles/tokens.css");
  return css.slice(css.indexOf("*/") + 2);
}

describe("the design tokens", () => {
  it("are the block of DESIGN_SPEC 7.1, character for character", () => {
    const block = specificationBlock();
    expect(block).toContain("--app-accent");
    expect(block).toContain("--hotend-part-8");
    expect(stylesheet().trim()).toBe(block.trim());
  });

  it("define both themes on the data-theme attribute", () => {
    const css = stylesheet();
    expect(css).toContain('[data-theme="dark"]');
    expect(css).toContain('[data-theme="light"]');
    expect(css).not.toContain("prefers-color-scheme");
  });
});

describe("the vendored components", () => {
  const folder = join(import.meta.dirname, "../src/components/ui/");
  const files = readdirSync(folder).filter((name) => /\.(tsx|ts|css)$/.test(name));

  it("exist", () => {
    expect(files).toContain("button.tsx");
  });

  it.each(files)("%s holds no colour literal", (name) => {
    const source = readFileSync(join(folder, name), "utf8");
    expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(source).not.toMatch(/\b(rgb|rgba|hsl|hsla|oklch|oklab|lab|lch|color-mix)\(/);
  });

  it("their class sheet holds no colour literal either", () => {
    const sheet = read("../src/styles/shadcn.css");
    expect(sheet).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(sheet).not.toMatch(/\b(rgb|rgba|hsl|hsla|oklch|oklab|lab|lch)\(/);
  });
});
