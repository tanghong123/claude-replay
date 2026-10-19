/** Native-only extension of the page storage boundary; exports never load this module. */
function boundary() {
  // The Monitor shell does not load the export page's storage shim.
  return globalThis.crPageStore ||= {
    tabGet(key) { try { return globalThis.sessionStorage.getItem(key); } catch { return null; } },
    tabSet(key, value) { try { globalThis.sessionStorage.setItem(key, value); } catch { /* The caller verifies readback. */ } },
  };
}
export function nativeDraftTab(createId) {
  const page = boundary(), key = "am-native-draft-tab";
  const existing = page.tabGet(key);
  if (existing) return existing;
  const id = createId(); page.tabSet(key, id);
  if (page.tabGet(key) !== id) throw new Error("This browser cannot retain native drafts across reload. Enable site storage before sending.");
  return id;
}
export function nativePageStore(page = boundary(), factory = globalThis.indexedDB) {
  if (!page) throw new Error("Native delivery storage is unavailable. No message was sent.");
  if (page.nativeDelivery) return page.nativeDelivery;
  let database;
  const open = () => database ||= new Promise((resolve, reject) => {
    if (!factory) { reject(new Error("This browser cannot preserve delivery state. No message was sent.")); return; }
    const request = factory.open("agent-monitor-native-delivery", 1);
    let blocked = false;
    request.onupgradeneeded = () => request.result.createObjectStore("scopes");
    request.onsuccess = () => {
      const db = request.result;
      if (blocked) { db.close(); return; }
      db.onversionchange = () => { db.close(); database = null; };
      resolve(db);
    };
    request.onerror = () => { database = null; reject(new Error("Open native delivery storage", { cause: request.error })); };
    request.onblocked = () => { blocked = true; database = null; reject(new Error("Close older Monitor tabs to open delivery storage.")); };
  });
  page.nativeDelivery = {
    /** One read/write transaction serializes delivery identity claims across browser tabs. */
    async change(scope, update) {
      const db = await open();
      return new Promise((resolve, reject) => {
        const transaction = db.transaction("scopes", "readwrite"), store = transaction.objectStore("scopes");
        let result, failure;
        const request = store.get(scope);
        request.onsuccess = () => {
          try {
            const value = request.result || { version: 1, attempts: [], drafts: {} };
            if (value.version !== 1 || !Array.isArray(value.attempts) || !value.drafts || typeof value.drafts !== "object") throw new Error("Unsupported native delivery journal. Preserve browser data and reconcile pending sends before continuing.");
            result = update(value);
            store.put(value, scope);
          } catch (error) { failure = error; transaction.abort(); }
        };
        transaction.oncomplete = () => resolve(result);
        transaction.onabort = () => reject(failure || new Error("Save native delivery state. No new send can start until browser storage is available.", { cause: transaction.error }));
        transaction.onerror = () => {}; // onabort retains the transaction's actual cause.
      });
    }
  };
  return page.nativeDelivery;
}
