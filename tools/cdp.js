// Drives Drop In's WebView2 over its debug port, so tests never touch the real mouse, keyboard or focus.
// Start the app first:
//   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333 target/release/dropin-app.exe --background
// node tools/cdp.js <step> [<step> ...]
//   steps: eval:<js>  click:<css>  rclick:<css>  dbl:<css>  key:<Key>  type:<text>  shot:<file>  wait:<ms>
//          drag:<css>@<dx>  pages  page:main|<url part>
// Needs Playwright: a global install, or PLAYWRIGHT_PATH pointing at its package folder.
const { chromium } = require(process.env.PLAYWRIGHT_PATH ?? 'playwright');

(async () => {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${process.env.CDP_PORT ?? 9333}`);
  const pages = () => browser.contexts().flatMap(c => c.pages());
  let page = pages().find(p => !p.url().includes('diff.html')) ?? pages()[0];
  for (const step of process.argv.slice(2)) {
    const at = step.indexOf(':');
    const [cmd, arg] = at < 0 ? [step, ''] : [step.slice(0, at), step.slice(at + 1)];
    if (cmd === 'page') page = pages().find(p => (arg === 'main' ? !p.url().includes('diff.html') : p.url().includes(arg))) ?? page;
    else if (cmd === 'pages') console.log(JSON.stringify(pages().map(p => p.url())));
    else if (cmd === 'eval') console.log(JSON.stringify(await page.evaluate(arg)));
    else if (cmd === 'click') await page.click(arg);
    else if (cmd === 'rclick') await page.click(arg, { button: 'right' });
    else if (cmd === 'dbl') await page.dblclick(arg);
    else if (cmd === 'key') await page.keyboard.press(arg);
    else if (cmd === 'type') await page.keyboard.type(arg);
    else if (cmd === 'shot') await page.screenshot({ path: arg, timeout: 8000 });
    else if (cmd === 'wait') await page.waitForTimeout(Number(arg));
    else if (cmd === 'drag') {
      // drag:<css>@<dx>: press on the element's middle and move it dx pixels sideways.
      const [css, dx] = arg.split('@');
      const box = await page.locator(css).first().boundingBox();
      const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x + Number(dx), y, { steps: 5 });
      await page.mouse.up();
    }
    else throw new Error(`unknown step ${step}`);
  }
  process.exit(0);
})().catch(error => {
  console.error(String(error));
  process.exit(1);
});
