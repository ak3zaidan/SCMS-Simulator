import { chromium } from '@playwright/test';
const OUT='/private/tmp/claude-501/-Users-ahmedzaidan-Developer-SCMS-Simulator/9f0649d7-8535-468d-9a82-3700cd13b998/scratchpad/shots';
const b = await chromium.launch({ headless: true, args: ['--use-gl=angle','--use-angle=swiftshader','--enable-unsafe-swiftshader'] });
const p = await b.newPage({ viewport: { width: 1280, height: 800 } });
await p.goto('http://127.0.0.1:5173/');
await p.waitForSelector('[data-testid="viewer-canvas"]');
await p.evaluate(async()=>{const e=window.__vwpStudio.engine; await e.request('run.start',{}).catch(()=>{}); await e.request('run.resume',{}).catch(()=>{});});
await p.waitForTimeout(3000);
await p.evaluate(()=>{const ps=window.__vwpStudio.engine.client.poses; for(let s=0;s<ps.count;s++) if(ps.occupied[s]===1){window.__vwpStudio.engine.selectActor(ps.actorId[s],'chase');break;}});
for (let i=0;i<12;i++){
  await p.waitForTimeout(1500);
  const s = await p.evaluate(()=>{const v=window.__vwpStudio.engine.viewer;const cs=v.cameras.state();
    return {t:document.querySelector('[data-testid="sim-clock"]')?.textContent, mode:cs.mode, st:v.actors.stats, snapInst:v.stats.snapshot().actorInstances,
      camz:+cs.position.z.toFixed(2), d:+cs.distanceM.toFixed(1), stalled: v.lastFrame.stalled};});
  console.log(i, JSON.stringify(s));
}
await p.screenshot({path:`${OUT}/chase-settled.png`});
await b.close();
