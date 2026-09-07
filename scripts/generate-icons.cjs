#!/usr/bin/env node
// Regenerate committed runtime icons from the approved SVG artwork.
// Install the pinned renderer as documented in assets/icons/README.md.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

if (process.platform !== 'darwin') {
  throw new Error('Regenerate icons on macOS: the ICNS file requires Apple iconutil.');
}

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
  // IconServices needs native encodings for the small icon slots. Handwritten
  // icp4/icp5 PNG chunks misdecode at 16/32 px, including in Control Center.
  const representations = [
    ['icon_16x16.png', 16], ['icon_16x16@2x.png', 32],
    ['icon_32x32.png', 32], ['icon_32x32@2x.png', 64],
    ['icon_128x128.png', 128], ['icon_128x128@2x.png', 256],
    ['icon_256x256.png', 256], ['icon_256x256@2x.png', 512],
    ['icon_512x512.png', 512], ['icon_512x512@2x.png', 1024],
  ];
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'macindecode-icons-'));
  try {
    const iconset = path.join(temporary, 'AppIcon.iconset');
    fs.mkdirSync(iconset);
    for (const [filename, size] of representations) {
      fs.writeFileSync(path.join(iconset, filename), images.get(size));
    }
    const output = path.join(temporary, 'AppIcon.icns');
    execFileSync('/usr/bin/iconutil', ['-c', 'icns', '-o', output, iconset]);
    return fs.readFileSync(output);
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
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
