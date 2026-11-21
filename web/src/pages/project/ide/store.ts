import type { Drafts } from "./changes";

// The web IDE's unsaved work, kept in this browser (IndexedDB) per user, project and ref,
// so a reload or a closed tab loses nothing. Storage that is missing or refused
// (a private window, site data blocked) is not an error: the work then lives as
// long as the page.

/** What is kept for one project and ref. */
export type Saved = {
  /** the commit the changes were made on */
  base: string;
  drafts: Drafts;
};

const DB = "forgust-ide";
const STORE = "drafts";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
    req.onblocked = () => reject(new Error("blocked"));
  });
}

async function run<T>(mode: IDBTransactionMode, work: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await open();
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = db.transaction(STORE, mode);
      const req = work(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(req.result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

/** Whose work, on what: another account in the same browser has its own. */
export type Desk = { userId: number; projectId: number; ref: string };

const key = (d: Desk) => `${d.userId}:${d.projectId}:${d.ref}`;

/** The work kept for a project's ref, or null (none, or no storage). */
export async function loadDrafts(desk: Desk): Promise<Saved | null> {
  try {
    return (await run<Saved | undefined>("readonly", (s) => s.get(key(desk)))) ?? null;
  } catch {
    return null;
  }
}

/** Keep the work of a project's ref; with nothing left, forget it. */
export async function saveDrafts(desk: Desk, base: string, drafts: Drafts): Promise<void> {
  try {
    const k = key(desk);
    if (Object.keys(drafts).length === 0) await run("readwrite", (s) => s.delete(k));
    else await run("readwrite", (s) => s.put({ base, drafts } satisfies Saved, k));
  } catch {
    // no storage: nothing to do about it
  }
}
