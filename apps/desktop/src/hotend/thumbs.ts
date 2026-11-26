// Pictures for the project cards (DESIGN_SPEC 10.6): the stage as it looked the last time
// a version was on screen in Design, scaled to 528 x 352 and kept in the window's own
// IndexedDB, one per theme, newest 200. It is a cache for display: nothing else reads it and nothing is
// lost when it is cleared.
const DATABASE = "hotend.thumbs";
const STORE = "pictures";
const KEEP = 200;
export const THUMB_SIZE = [528, 352] as const;

type Row = { key: string; at: number; picture: string };

function open(): Promise<IDBDatabase | null> {
  return new Promise((resolve) => {
    try {
      const request = indexedDB.open(DATABASE, 1);
      request.onupgradeneeded = () => {
        request.result.createObjectStore(STORE, { keyPath: "key" }).createIndex("at", "at");
      };
      request.onsuccess = () => {
        resolve(request.result);
      };
      request.onerror = () => {
        resolve(null);
      };
    } catch {
      resolve(null);
    }
  });
}

export type ThumbTheme = "dark" | "light";

/** One picture per version and theme: a part lit for the dark stage is wrong on a light card. */
export function thumbKey(projectId: string, version: number, theme: ThumbTheme): string {
  // "-3": the part tints changed twice; pictures in the old tints are not shown again
  // (they fall out of the cache with the oldest).
  return `${projectId}:${version}:${theme}-3`;
}

/** The picture of a version for a theme, or null when it was never on the stage. */
export async function readThumb(projectId: string, version: number, theme: ThumbTheme): Promise<string | null> {
  const database = await open();
  if (!database) return null;
  return new Promise((resolve) => {
    const request = database.transaction(STORE).objectStore(STORE).get(thumbKey(projectId, version, theme));
    request.onsuccess = () => {
      resolve((request.result as Row | undefined)?.picture ?? null);
    };
    request.onerror = () => {
      resolve(null);
    };
  });
}

/** Scales a picture of the stage to the card's size, cropped to its 3:2 middle. */
export function scaleThumb(dataUrl: string): Promise<string | null> {
  return new Promise((resolve) => {
    const image = new Image();
    image.onload = () => {
      const [width, height] = THUMB_SIZE;
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d");
      if (!context) {
        resolve(null);
        return;
      }
      const scale = Math.max(width / image.width, height / image.height);
      const w = image.width * scale;
      const h = image.height * scale;
      context.drawImage(image, (width - w) / 2, (height - h) / 2, w, h);
      resolve(canvas.toDataURL("image/png"));
    };
    image.onerror = () => {
      resolve(null);
    };
    image.src = dataUrl;
  });
}

/** Keeps the picture of a version and drops the oldest beyond the newest 200. */
export async function saveThumb(projectId: string, version: number, theme: ThumbTheme, picture: string): Promise<void> {
  const database = await open();
  if (!database) return;
  await new Promise<void>((resolve) => {
    const transaction = database.transaction(STORE, "readwrite");
    const store = transaction.objectStore(STORE);
    store.put({ key: thumbKey(projectId, version, theme), at: Date.now(), picture } satisfies Row);
    const count = store.count();
    count.onsuccess = () => {
      let extra = count.result - KEEP;
      if (extra <= 0) return;
      const cursor = store.index("at").openCursor();
      cursor.onsuccess = () => {
        if (extra <= 0 || !cursor.result) return;
        cursor.result.delete();
        extra -= 1;
        cursor.result.continue();
      };
    };
    transaction.oncomplete = () => {
      resolve();
    };
    transaction.onerror = () => {
      resolve();
    };
  });
}
