import { chromium } from '@playwright/test';
const OUT = process.env.OUT ?? '/private/tmp/claude-501/-Users-ahmedzaidan-Developer-SCMS-Simulator/9f0649d7-8535-468d-9a82-3700cd13b998/scratchpad/shots';
const URL = process.env.URL ?? 'http://127.0.0.1:5173/';
const b = await chromium.launch({ headless: true, args: ['--use-gl=angle','--use-angle=swiftshader','--enable-unsafe-swiftshader','--ignore-gpu-blocklist'] });
const p = await b.newPage({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 });
const errs = [];
p.on('console', m => { if (m.type()==='error') errs.push(m.text()); });
p.on('pageerror', e => errs.push('pageerror: '+e.message));
await p.goto(URL, { waitUntil: 'domcontentloaded' });
await p.waitForSelector('[data-testid="viewer-canvas"]', { timeout: 30000 });
await p.waitForTimeout(4000);
const dump = async (tag) => {
  const st = await p.evaluate(() => {
    const api = window.__vwpStudio; const e = api?.engine;
    const t = (s) => document.querySelector(`[data-testid="${s}"]`)?.textContent ?? null;
    const v = e?.viewer;
    return {
      conn: document.querySelector('[data-testid="connection-state"]')?.getAttribute('data-state'),
      poses: e?.client?.poses?.count ?? null,
      occupied: e?.client?.poses?.occupied ? Array.from(e.client.poses.occupied).reduce((a,x)=>a+(x===1?1:0),0) : null,
      helloNodeCount: e?.client?.hello?.nodeCount ?? null,
      nodesMapSize: e?.nodes?.size ?? null,
      stats: v?.stats?.snapshot?.() ?? null,
      cam: v?.cameras?.state?.() ?? null,
      clock: t('sim-clock'), follow: t('follow-chip'),
      worldLanes: e?.world?.lanes?.count ?? null,
      worldBuildings: e?.world?.buildings?.count ?? null,
    };
  });
  console.log('##', tag, JSON.stringify(st, (k,v)=> typeof v==='number'? Number(v.toFixed?Number(v.toFixed(3)):v) : v, 1));
  await p.screenshot({ path: `${OUT}/${tag}.png` });
};
await dump('a-initial');
// resume / restart
await p.evaluate(async()=>{ const e=window.__vwpStudio?.engine; await e?.request('run.start',{}).catch(()=>{}); await e?.request('run.resume',{}).catch(()=>{}); });
await p.waitForTimeout(5000);
await dump('b-running');
const picked = await p.evaluate(()=>{ const e=window.__vwpStudio?.engine; const ps=e?.client?.poses; if(!ps) return null; for(let s=0;s<ps.count;s++){ if(ps.occupied[s]===1){ const id=ps.actorId[s]; e.selectActor(id,'chase'); return {id, x:ps.x?.[s], y:ps.y?.[s]}; } } return null; });
console.log('## picked', JSON.stringify(picked));
await p.waitForTimeout(5000);
await dump('c-chase');
await p.evaluate(()=>window.__vwpStudio?.engine?.viewer?.cameras?.setMode?.('dashboard'));
await p.waitForTimeout(3000);
await dump('d-dashboard');
console.log('## errors', JSON.stringify(errs.slice(0,10)));
await b.close();
