// Renders film.html frame by frame. Runs in the Playwright image; see render.sh.
// Usage: node render.mjs preview 3 22 41   (stills at those seconds)
//        node render.mjs film
import { chromium } from "playwright";

const SITE = "http://localhost:8000";
const OUT = "/out";
const FPS = 30;
const [mode, ...rest] = process.argv.slice(2);

const browser = await chromium.launch();

// A real capture of the data room, reading testnet live.
const room = await browser.newPage({ viewport: { width: 1040, height: 1000 } });
await room.goto(`${SITE}/web/index.html?contract=v1.deed-hq.testnet&deed=desk-5`);
await room.waitForSelector("#room:not([hidden])", { timeout: 60000 });
await room.addStyleTag({ content: ".picker, .lede, h1 { display: none !important; } main { padding-top: 0 !important; } #status { display: none; } footer { display: none; }" });
await room.screenshot({ path: `${OUT}/dataroom.png`, fullPage: true });
await room.close();

const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
page.on("pageerror", (e) => console.log("pageerror", e.message));
await page.route("**/dataroom.png", (route) => route.fulfill({ path: `${OUT}/dataroom.png` }));
await page.goto(`${SITE}/video/film.html`);
await page.evaluate(() => document.fonts.ready);
await page.evaluate(() => Promise.all([...document.images].map((i) => i.decode())));
const overflow = async () =>
  page.evaluate(() => {
    const bad = [];
    document.querySelectorAll(".scene").forEach((s) => {
      if (s.style.display === "none") return;
      s.querySelectorAll(".a, .log, .figures").forEach((n) => {
        const r = n.getBoundingClientRect();
        if (n.closest(".room")) return;
        if (r.height && (r.bottom > 1080 || r.right > 1920)) bad.push(`${n.textContent.trim().slice(0, 40)} bottom=${Math.round(r.bottom)}`);
      });
    });
    return bad;
  });

if (mode === "preview") {
  for (const t of rest.map(Number)) {
    await page.evaluate((x) => window.seek(x), t);
    console.log(t, await overflow());
    await page.screenshot({ path: `${OUT}/still-${t}.jpg`, type: "jpeg", quality: 70 });
  }
} else {
  const total = Math.round((await page.evaluate(() => window.DURATION)) * FPS);
  for (let frame = 0; frame < total; frame++) {
    await page.evaluate((x) => window.seek(x), frame / FPS);
    await page.screenshot({ path: `${OUT}/frames/${String(frame).padStart(5, "0")}.jpg`, type: "jpeg", quality: 92 });
    if (frame % 300 === 0) console.log(`frame ${frame}/${total}`);
  }
}
await browser.close();
