import { chromium } from '@playwright/test';
const b = await chromium.launch({ headless: true, args: ['--use-gl=angle','--use-angle=swiftshader','--enable-unsafe-swiftshader'] });
const p = await b.newPage({ viewport: { width: 1280, height: 800 } });
await p.goto('http://127.0.0.1:5173/');
await p.waitForSelector('[data-testid="viewer-canvas"]');
await p.evaluate(async()=>{const e=window.__vwpStudio.engine; await e.request('run.start',{}).catch(()=>{}); await e.request('run.resume',{}).catch(()=>{});});
await p.waitForTimeout(4000);
const read = () => p.evaluate(()=>{
  const v=window.__vwpStudio.engine.viewer, it=v.interpolator;
  const f=(n)=>+n.toFixed(2);
  return { mode: v.cameras.state().mode, actorStats: v.actors.stats, interpCount: it.count,
    occ: Array.from(it.outOccupied.slice(0,3)), pos: Array.from(it.outPosition.slice(0,3)).map(f),
    hidden: v.actors.hiddenActorId, cam: v.cameras.state().position,
    camNear: v.camera.near, camFar: v.camera.far, look: v.cameras.state().target };
});
console.log('MAP', JSON.stringify(await read()));
await p.evaluate(()=>{const ps=window.__vwpStudio.engine.client.poses; for(let s=0;s<ps.count;s++) if(ps.occupied[s]===1){window.__vwpStudio.engine.selectActor(ps.actorId[s],'chase');break;}});
await p.waitForTimeout(3500);
console.log('CHASE', JSON.stringify(await read()));
for (const m of ['dashboard','free','map']) { await p.evaluate(mm=>window.__vwpStudio.engine.viewer.cameras.setMode(mm), m); await p.waitForTimeout(2500); console.log(m.toUpperCase(), JSON.stringify(await read())); }
await b.close();
