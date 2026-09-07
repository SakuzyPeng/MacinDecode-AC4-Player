// Preserve CSS-driven opacity, transforms, and custom properties on <use>.
// In particular, initially transparent front/back layers are not dead content.
export default {
  multipass: true,
  plugins: [{name: 'preset-default', params: {overrides: {
    removeHiddenElems: false,
    collapseGroups: false,
    moveElemsAttrsToGroup: false,
    moveGroupAttrsToElems: false,
    convertTransform: false,
    inlineStyles: false,
    cleanupIds: false,
    // Separate antialiased strokes composite differently where they overlap.
    mergePaths: false,
  }}}],
};
