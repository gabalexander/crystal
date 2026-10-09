// Screenshots of the page in headless Chrome, to compare with Code Wiki's:
//     node assets/wiki/dev/shoot.mjs http://127.0.0.1:8765/p/crystal/ /tmp/shots/page [scenarios...]
// The scenarios: desktop, sub (a subsection's diagram), zoom, light, phone (and its outline), chat.
import { launch } from './cdp.mjs';

const [url, out, ...which] = process.argv.slice(2);
const want = (name) => !which.length || which.includes(name);
const page = await launch();
try {
  if (want('desktop')) {
    await page.viewport(1568, 900);
    await page.scheme('dark');
    await page.goto(url, 2500);
    await page.shot(`${out}-desktop.jpg`);
  }
  if (want('sub')) {
    await page.viewport(1568, 900);
    await page.scheme('dark');
    const id = await page.eval(`(location.href, crystalWiki.state.entries.find(e => e.level === 3 && e.section === 0)?.id)`) ;
    await page.eval(`(() => { const e = crystalWiki.state.entries.filter(e => e.level === 3)[1]; document.getElementById(e.id).scrollIntoView({block:'start'}); })()`);
    await page.sleep(2500);
    await page.shot(`${out}-sub.jpg`);
  }
  if (want('zoom')) {
    await page.eval(`(() => { const c = [...document.querySelectorAll('.diagram-card.drawn')].find(c => { const r = c.getBoundingClientRect(); return r.top > 0 && r.top < innerHeight; }) || document.querySelector('.diagram-card.drawn'); c.click(); })()`);
    await page.sleep(800);
    await page.shot(`${out}-zoom.jpg`);
    await page.key('Escape', 'Escape');
    await page.sleep(300);
  }
  if (want('light')) {
    await page.viewport(1568, 900);
    await page.scheme('light');
    await page.eval(`localStorage.setItem('crystal-wiki-theme','light')`);
    await page.goto(url, 2500);
    await page.eval(`(() => { const e = crystalWiki.state.entries.filter(e => e.level === 3)[1]; document.getElementById(e.id).scrollIntoView({block:'start'}); })()`);
    await page.sleep(2500);
    await page.shot(`${out}-light.jpg`);
    await page.eval(`localStorage.removeItem('crystal-wiki-theme')`);
  }
  if (want('phone')) {
    await page.scheme('dark');
    await page.viewport(390, 844, true, 2);
    await page.goto(url, 2500);
    await page.shot(`${out}-phone.jpg`);
    await page.eval(`document.getElementById('cw-outline-btn').click()`);
    await page.sleep(500);
    await page.shot(`${out}-phone-outline.jpg`);
  }
  if (want('chat')) {
    await page.viewport(1568, 900);
    await page.scheme('dark');
    await page.goto(url, 2000);
    await page.eval(`(() => { const i = document.getElementById('cw-chat-input'); i.value = 'How does the daemon hand itself over?'; i.dispatchEvent(new Event('input')); document.getElementById('cw-chat-form').requestSubmit(); })()`);
    await page.sleep(6000);
    await page.shot(`${out}-chat.jpg`);
  }
} finally {
  if (page.console.length) console.log(page.console.join('\n'));
  await page.close();
}
