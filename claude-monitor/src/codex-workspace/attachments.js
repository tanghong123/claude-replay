/**
 * Image drafts stay in memory and belong to the thread captured before a file read.
 * @typedef {{name:string, type:string, size:number, arrayBuffer:()=>Promise<ArrayBuffer>}} ImageFile
 * @typedef {{id:number, name:string, size:number, status:"reading"} | {id:number, name:string, size:number, status:"ready", url:string}} ImageDraft
 * @typedef {{id:number, url:string}} ImageInput
 * @typedef {(file:ImageFile)=>Promise<string>} ImageReader
 */
export const IMAGE_LIMITS = Object.freeze({ count: 4, fileBytes: 4 * 1024 * 1024, totalBytes: 8 * 1024 * 1024 });
export const IMAGE_TYPES = Object.freeze(["image/png", "image/jpeg", "image/webp", "image/gif"]);

function matchesType(bytes, type) {
  const ascii = (start, end) => String.fromCharCode(...bytes.subarray(start, end));
  return type === "image/png" ? bytes.length >= 8 && [137,80,78,71,13,10,26,10].every((byte, i) => bytes[i] === byte)
    : type === "image/jpeg" ? bytes.length >= 3 && bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255
    : type === "image/gif" ? ["GIF87a", "GIF89a"].includes(ascii(0, 6))
    : type === "image/webp" && bytes.length >= 12 && ascii(0, 4) === "RIFF" && ascii(8, 12) === "WEBP";
}

/**
 * Read bounded raster bytes into a native inline URL. Reject invalid size/type/signature;
 * filesystem read failures retain the original rejection cause.
 * @param {ImageFile} file
 * @returns {Promise<string>}
 */
export async function readImageFile(file) {
  if (!IMAGE_TYPES.includes(file.type)) throw new Error("Choose a PNG, JPEG, WebP or GIF image.");
  if (!Number.isSafeInteger(file.size) || file.size <= 0 || file.size > IMAGE_LIMITS.fileBytes) throw new Error("Each image must be between 1 byte and 4 MiB.");
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (bytes.length !== file.size || !matchesType(bytes, file.type)) throw new Error("Image contents do not match PNG, JPEG, WebP or GIF format.");
  let binary = "";
  for (let start = 0; start < bytes.length; start += 32768) binary += String.fromCharCode(...bytes.subarray(start, start + 32768));
  return `data:${file.type};base64,${btoa(binary)}`;
}

/** Public boundary for asynchronous reads, reservations, snapshots and accepted-send retirement. */
export class ImageAttachmentDrafts {
  /** @param {ImageReader} read @param {(threadId:string)=>void} changed */
  constructor(read = readImageFile, changed = () => {}) { this.read = read; this.changed = changed; this.threads = new Map(); this.nextId = 0; }
  /** @param {string|null} threadId @returns {ImageDraft[]} Current reserved/ready drafts. */
  items(threadId) { return this.threads.get(threadId) || []; }
  /** @param {string} threadId @param {number} id @returns {void} Retire a draft, including a pending read. */
  remove(threadId, id) {
    const remaining = this.items(threadId).filter(item => item.id !== id);
    if (remaining.length) this.threads.set(threadId, remaining); else this.threads.delete(threadId);
    this.changed(threadId);
  }
  /** @param {string} threadId @returns {ImageInput[]} Stable inputs; throws while any read is pending. */
  snapshot(threadId) {
    const items = this.items(threadId);
    if (items.some(item => item.status === "reading")) throw new Error("Wait for the images to finish loading.");
    return items.map(item => ({ id: item.id, url: item.url }));
  }
  /** @param {string} threadId @param {ImageInput[]} sent @returns {void} Remove only accepted inputs. */
  accepted(threadId, sent) { for (const item of sent) this.remove(threadId, item.id); }
  /**
   * Reserve/read files for the captured thread. Removed reservations never reappear.
   * @param {string} threadId
   * @param {Iterable<ImageFile>} files
   * @returns {Promise<Error[]>} File-scoped errors retaining read causes; rejects a missing thread.
   */
  async add(threadId, files) {
    if (!threadId) throw new Error("Choose a Codex session before attaching an image.");
    const reads = [], errors = [];
    for (const file of files) {
      const items = this.items(threadId);
      const reason = !IMAGE_TYPES.includes(file.type) ? "Choose a PNG, JPEG, WebP or GIF image."
        : !Number.isSafeInteger(file.size) || file.size <= 0 || file.size > IMAGE_LIMITS.fileBytes ? "Each image must be between 1 byte and 4 MiB."
        : items.length >= IMAGE_LIMITS.count ? "Attach at most 4 images per message."
        : items.reduce((total, item) => total + item.size, 0) + file.size > IMAGE_LIMITS.totalBytes ? "Images in one message must total at most 8 MiB." : null;
      if (reason) { errors.push(new Error(`${file.name || "Image"}: ${reason}`)); continue; }
      const item = { id: ++this.nextId, name: file.name || "Pasted image", size: file.size, status: "reading" };
      this.threads.set(threadId, [...items, item]); this.changed(threadId);
      reads.push((async () => {
        try {
          const url = await this.read(file);
          if (!this.items(threadId).includes(item)) return;
          Object.assign(item, { status: "ready", url }); this.changed(threadId);
        } catch (error) {
          if (!this.items(threadId).includes(item)) return;
          this.remove(threadId, item.id); errors.push(new Error(`${item.name}: ${error?.message || "Could not read image."}`, { cause: error }));
        }
      })());
    }
    await Promise.all(reads);
    return errors;
  }
}
