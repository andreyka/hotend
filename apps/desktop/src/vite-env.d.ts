/// <reference types="vite/client" />

/**
 * True only in the bridge build (`vite build --mode bridge`), which runs in a normal
 * browser against the development bridge. False in the window's build, where every
 * branch behind it is removed.
 */
declare const __HOTEND_BRIDGE__: boolean;

/** [name, version, licence] of every runtime dependency, from docs/DEPENDENCIES.md at build time. */
declare const __HOTEND_LICENCES__: [string, string, string][];

/** The viewer's stylesheet; the bundler puts it into the window's one CSS file. */
declare module "three-cad-viewer/css";
