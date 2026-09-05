#!/usr/bin/env node
// Regenerate committed runtime icons from the approved SVG artwork.
// Install the pinned renderer as documented in assets/icons/README.md.
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '..');
const { Resvg } = require(path.join(root, 'target/icon-tools/node_modules/@resvg/resvg-js'));
const directory = path.join(root, 'assets/icons');

function renderSizes(filename, sizes) {
  const svg = fs.readFileSync(path.join(directory, filename), 'utf8');
  return new Map(sizes.map(size => [size, Buffer.from(new Resvg(svg, {
    fitTo: { mode: 'width', value: size },
    font: { loadSystemFonts: false },
  }).render().asPng())]));
}

function windowsIcon(images) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(1, 2); // ICO, not CUR.
  header.writeUInt16LE(images.size, 4);
  let offset = 6 + images.size * 16;
  const entries = [];
  for (const [size, png] of images) {
    const entry = Buffer.alloc(16);
    entry[0] = size === 256 ? 0 : size;
    entry[1] = entry[0];
    entry.writeUInt16LE(1, 4);
    entry.writeUInt16LE(32, 6);
    entry.writeUInt32LE(png.length, 8);
    entry.writeUInt32LE(offset, 12);
    entries.push(entry);
    offset += png.length;
  }
  return Buffer.concat([header, ...entries, ...images.values()]);
}

function macIcon(images) {
  // PNG-backed ICNS representations supported by the app's macOS 14 minimum.
  const representations = [
    ['icp4', 16], ['ic11', 32], ['icp5', 32], ['ic12', 64],
    ['ic07', 128], ['ic13', 256], ['ic08', 256], ['ic14', 512],
    ['ic09', 512], ['ic10', 1024],
  ];
  const chunks = representations.map(([type, size]) => {
    const png = images.get(size);
    const header = Buffer.alloc(8);
    header.write(type, 0, 'ascii');
    header.writeUInt32BE(png.length + 8, 4);
    return Buffer.concat([header, png]);
  });
  const header = Buffer.alloc(8);
  header.write('icns', 0, 'ascii');
  header.writeUInt32BE(8 + chunks.reduce((sum, chunk) => sum + chunk.length, 0), 4);
  return Buffer.concat([header, ...chunks]);
}

const mac = renderSizes('app-macos.svg', [16, 32, 64, 128, 256, 512, 1024]);
const windows = renderSizes('app-windows.svg', [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]);
for (const [filename, bytes] of [
  ['app-macos.png', mac.get(256)],
  ['app-windows.png', windows.get(256)],
  ['app-macos.icns', macIcon(mac)],
  ['app-windows.ico', windowsIcon(windows)],
]) {
  fs.writeFileSync(path.join(directory, filename), bytes);
  console.log(`${filename}: ${bytes.length} bytes`);
}
