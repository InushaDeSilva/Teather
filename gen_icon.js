const zlib = require('zlib');
const fs = require('fs');

function makePNG(w, h) {
    const d = [];
    const cx1 = w * 0.34, cy1 = h * 0.39; // left circle center
    const cx2 = w * 0.66, cy2 = h * 0.59; // right circle center
    const r = w * 0.137;                    // circle radius
    const cornerR = w * 0.195;              // rounded corner radius
    const lineW = w * 0.039;               // line width

    for (let y = 0; y < h; y++) {
        d.push(0); // filter byte
        for (let x = 0; x < w; x++) {
            // ── Background: rounded rect with indigo→violet gradient ──
            const inRoundedRect = isInRoundedRect(x, y, w, h, cornerR);

            if (!inRoundedRect) {
                d.push(0, 0, 0, 0); // transparent outside
                continue;
            }

            // Gradient: top-left indigo → bottom-right violet
            const t = (x / w + y / h) / 2;
            const bgR = Math.floor(99 + t * 40);   // 99→139
            const bgG = Math.floor(102 - t * 40);  // 102→62
            const bgB = Math.floor(241 - t * 30);  // 241→211

            // ── Foreground: two circles + connecting line ──
            const d1 = Math.sqrt((x - cx1) ** 2 + (y - cy1) ** 2);
            const d2 = Math.sqrt((x - cx2) ** 2 + (y - cy2) ** 2);

            // Distance to line segment between circle centers
            const dLine = distToSegment(x, y, cx1, cy1, cx2, cy2);

            const inCircle1 = d1 <= r;
            const inCircle2 = d2 <= r;
            const inLine = dLine <= lineW && !inCircle1 && !inCircle2;

            if (inCircle1 || inCircle2) {
                // White circles
                d.push(255, 255, 255, 230);
            } else if (inLine) {
                // Solid white line
                d.push(255, 255, 255, 210);
            } else {
                d.push(bgR, bgG, bgB, 255);
            }
        }
    }

    const raw = Buffer.from(d);
    const compressed = zlib.deflateSync(raw);
    const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

    function chunk(type, data) {
        const len = Buffer.alloc(4);
        len.writeUInt32BE(data.length);
        const t = Buffer.from(type);
        const combined = Buffer.concat([t, data]);
        const c = zlib.crc32(combined);
        const crc = Buffer.alloc(4);
        crc.writeUInt32BE(c >>> 0);
        return Buffer.concat([len, t, data, crc]);
    }

    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(w, 0);
    ihdr.writeUInt32BE(h, 4);
    ihdr[8] = 8;
    ihdr[9] = 6; // RGBA

    return Buffer.concat([
        sig,
        chunk('IHDR', ihdr),
        chunk('IDAT', compressed),
        chunk('IEND', Buffer.alloc(0))
    ]);
}

function isInRoundedRect(x, y, w, h, r) {
    // Check if point is inside a rounded rectangle
    if (x < r && y < r) return Math.sqrt((x - r) ** 2 + (y - r) ** 2) <= r;
    if (x > w - r && y < r) return Math.sqrt((x - (w - r)) ** 2 + (y - r) ** 2) <= r;
    if (x < r && y > h - r) return Math.sqrt((x - r) ** 2 + (y - (h - r)) ** 2) <= r;
    if (x > w - r && y > h - r) return Math.sqrt((x - (w - r)) ** 2 + (y - (h - r)) ** 2) <= r;
    return true;
}

function distToSegment(px, py, x1, y1, x2, y2) {
    const dx = x2 - x1, dy = y2 - y1;
    const lenSq = dx * dx + dy * dy;
    let t = Math.max(0, Math.min(1, ((px - x1) * dx + (py - y1) * dy) / lenSq));
    const projX = x1 + t * dx, projY = y1 + t * dy;
    return Math.sqrt((px - projX) ** 2 + (py - projY) ** 2);
}

fs.mkdirSync('src-tauri/icons', { recursive: true });
fs.writeFileSync('src-tauri/icons/icon.png', makePNG(256, 256));
fs.writeFileSync('src-tauri/icons/32x32.png', makePNG(32, 32));
fs.writeFileSync('src-tauri/icons/128x128.png', makePNG(128, 128));
fs.writeFileSync('src-tauri/icons/128x128@2x.png', makePNG(256, 256));
fs.writeFileSync('src-tauri/icons/icon.ico', makePNG(256, 256));
fs.writeFileSync('src-tauri/icons/icon.icns', makePNG(256, 256));
console.log('All icons generated — two circles + tether line on indigo gradient');
