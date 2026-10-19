/**
 * Position shared phone controls in the visual viewport's CSS coordinates.
 * User pinch zoom owns the viewport pan; only unzoomed keyboard movement is compensated.
 * @param {number} layoutHeight Height of the transformed app containing block.
 * @param {{height:number, scale:number, pageTop:number}} view Current visual viewport.
 * @returns {{inset:number, shift:number, keyboardOpen:boolean}}
 * @throws {RangeError} When the app does not have a positive finite height.
 */
export function sharedKeyboardFrame(layoutHeight, view) {
  if (!Number.isFinite(layoutHeight) || layoutHeight <= 0) throw new RangeError("Phone app height must be positive and finite.");
  const height = Number.isFinite(view.height) && view.height > 0 ? view.height : layoutHeight;
  const scale = Number.isFinite(view.scale) && view.scale > 0 ? view.scale : 1;
  if (Math.abs(scale - 1) >= 0.01) return { inset: 0, shift: 0, keyboardOpen: false };
  const shift = Number.isFinite(view.pageTop) ? Math.max(0, view.pageTop) : 0;
  return {
    inset: Math.max(0, layoutHeight - height),
    shift,
    // Scale-normalized height distinguishes a keyboard from zoom alone.
    keyboardOpen: layoutHeight - height * scale > 80,
  };
}
