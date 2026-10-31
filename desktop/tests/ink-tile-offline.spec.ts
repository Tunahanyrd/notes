import {test,expect,type Page} from '@playwright/test';
import {spawn,spawnSync,type ChildProcess} from 'node:child_process';
import {chmod,cp,mkdir,mkdtemp,readFile,rm} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';

// Both engines use private copies of the synthetic demo library. A second
// engine represents a deterministic confirmed readback; the first remains
// offline and never sends anything to Samsung.
const root=path.resolve(import.meta.dirname,'../..');
const binary=process.env.NOTES_TILE_ENGINE??[path.join(root,'target/ink-tile-test/release/notes-engine'),path.join(root,'target/release/notes-engine'),path.join(root,'target/debug/notes-engine')].find(existsSync);
type Stage={home:string;data:string;child?:ChildProcess;runtime?:{url:string;token:string};origin?:string};
const sleep=(ms:number)=>new Promise(resolve=>setTimeout(resolve,ms));
async function prepareStage(work:string,name:string):Promise<Stage>{
  const home=path.join(work,name),data=path.join(home,'data');await mkdir(home,{mode:0o700});
  for(const dir of ['data','home','state','config','cache','share','run'])await mkdir(path.join(home,dir),{mode:0o700});
  await cp(path.join(root,'desktop/tests/fixtures/demo-library/library.sqlite3'),path.join(data,'library.sqlite3'));
  await chmod(path.join(data,'library.sqlite3'),0o600);return {home,data};
}
async function launch(stage:Stage){
  const dir=(name:string)=>path.join(stage.home,name);
  const env={PATH:process.env.PATH,HOME:dir('home'),XDG_STATE_HOME:dir('state'),XDG_CONFIG_HOME:dir('config'),XDG_CACHE_HOME:dir('cache'),XDG_DATA_HOME:dir('share'),XDG_RUNTIME_DIR:dir('run'),LINUX_NOTES_PROJECT_ROOT:root};
  stage.child=spawn(binary!,['--data-dir',stage.data,'--offline','--no-browser','--no-auto-sync','--port','0','--runtime-file',dir('runtime.json'),'--frontend-dir',path.join(root,'desktop/dist-test')],{cwd:stage.home,env,stdio:['ignore','ignore','pipe']});
  for(let i=0;i<300&&!stage.runtime;i++){try{stage.runtime=JSON.parse(await readFile(dir('runtime.json'),'utf8'));}catch{if(stage.child.exitCode!==null)throw Error('isolated engine exited');await sleep(100);}}
  if(!stage.runtime)throw Error('isolated engine did not start');stage.origin=new URL(stage.runtime.url).origin;
}
async function stop(stage:Stage|undefined){if(stage?.child&&stage.child.exitCode===null&&stage.child.signalCode===null){stage.child.kill('SIGTERM');await new Promise<void>(resolve=>stage.child!.once('exit',()=>resolve()));}}
async function get(stage:Stage,suffix:string){return fetch(stage.origin!+suffix,{headers:{Authorization:'Bearer '+stage.runtime!.token}});}
async function compare(page:Page,a:Buffer,b:Buffer){return page.evaluate(async({a,b})=>{
  const load=async(value:string)=>{const image=new Image();image.src='data:image/png;base64,'+value;await image.decode();return image;};
  const one=await load(a),two=await load(b);const left=document.createElement('canvas'),right=document.createElement('canvas');
  left.width=right.width=one.width;left.height=right.height=one.height;left.getContext('2d')!.drawImage(one,0,0);right.getContext('2d')!.drawImage(two,0,0);
  const x=left.getContext('2d')!.getImageData(0,0,left.width,left.height).data,y=right.getContext('2d')!.getImageData(0,0,right.width,right.height).data;
  let max=0,sum=0,changed=0;for(let i=0;i<x.length;i+=4){let pixel=0;for(let j=0;j<3;j++){const d=Math.abs(x[i+j]-y[i+j]);sum+=d;pixel=Math.max(pixel,d);max=Math.max(max,d);}if(pixel)changed++;}
  return {max,mean:sum/(x.length/4*3),changed};
},{a:a.toString('base64'),b:b.toString('base64')});}

test.use({viewport:{width:1600,height:1000},deviceScaleFactor:1.44});
for(const zoom of [100,150])test(`offline ink tiles preserve pixels at ${zoom}% and during handoff`,async({page},testInfo)=>{
  test.setTimeout(180_000);test.skip(!binary,'an isolated notes-engine binary is required');
  const work=await mkdtemp(path.join(tmpdir(),'notes-tiles-e2e-'));let first:Stage|undefined,second:Stage|undefined;
  let release!:()=>void;const gate=new Promise<void>(resolve=>{release=resolve;});let redirected=false,held=true,tileRequests=0;const redirectedRequests:string[]=[];
  try{
    first=await prepareStage(work,'first');await launch(first);
    const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
    await page.route('**/*',async route=>{
      const url=new URL(route.request().url());if(url.protocol==='blob:'||url.protocol==='data:')return route.continue();
      if(url.origin!==first!.origin)return route.abort();
      if(redirected&&route.request().method()==='GET'&&url.pathname.includes('/api/notes/')&&(/\/preview$|\/pages$|\/ink$|\/ink-pens$/.test(url.pathname)||/^\/api\/notes\/[^/]+$/.test(url.pathname))){
        redirectedRequests.push(url.pathname.split('/').pop()!+url.search);
        if(url.searchParams.get('layer')==='ink'&&held){tileRequests++;await gate;}
        const response=await get(second!,url.pathname+url.search),body=Buffer.from(await response.arrayBuffer()),headers=Object.fromEntries(response.headers);
        delete headers['content-encoding'];delete headers['content-length'];return route.fulfill({status:response.status,headers,body});
      }
      return route.continue();
    });
    const notes=await (await get(first,'/api/notes')).json() as {notes:{id:string;title:string}[]};
    const note=notes.notes.find(item=>item.title==='Linear algebra – week 3');expect(note).toBeTruthy();
    await page.goto(first.runtime!.url);await page.getByRole('button',{name:/Linear algebra – week 3/}).first().click();
    const pen=page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true});await expect(pen).toBeEnabled({timeout:90000});await pen.click();
    await expect(page.locator('.ink-tile').first()).toBeVisible({timeout:120000});
    if(zoom===150){
      await page.evaluate(()=>{const w=window as any;w.__tileFrames=[];w.__tileSampling=true;const tick=()=>{if(!w.__tileSampling)return;const box=document.querySelector<HTMLElement>('.ink-page');const tiles=[...document.querySelectorAll<HTMLCanvasElement>('.ink-tile')];const r=box?.getBoundingClientRect();const scroll=box?.closest('.canvas-scroll')?.getBoundingClientRect();let visible=0,ink=false,maxOffset=0;
        const bucket=Number(box?.dataset.inkBucket);for(const canvas of tiles){const c=canvas.getBoundingClientRect();if(!scroll||!r||!bucket||c.right<=scroll.left||c.left>=scroll.right||c.bottom<=scroll.top||c.top>=scroll.bottom)continue;visible++;const [column,row]=(canvas.dataset.tile??'').split(',').map(Number);maxOffset=Math.max(maxOffset,Math.abs(c.left-(r.left+column*512/bucket/1080*r.width)),Math.abs(c.top-(r.top+row*512/bucket/1527*r.height)));const data=canvas.getContext('2d')!.getImageData(0,0,canvas.width,canvas.height).data;for(let i=3;i<data.length;i+=16)if(data[i]){ink=true;break;}}
        w.__tileFrames.push({visible,ink,maxOffset,width:r?.width??0,height:r?.height??0});requestAnimationFrame(tick);};requestAnimationFrame(tick);});
      for(let i=0;i<5;i++){await page.getByRole('button',{name:'Yakınlaştır'}).click();await page.screenshot({path:testInfo.outputPath(`zoom-${i}.png`)});}
      await expect(page.locator('.ink-page').first()).toHaveAttribute('data-ink-bucket','1.5',{timeout:120000});
      const samples=await page.evaluate(()=>{const w=window as any;w.__tileSampling=false;return w.__tileFrames as {visible:number;ink:boolean;maxOffset:number;width:number;height:number}[];});
      expect(samples.length).toBeGreaterThan(4);expect(samples.every(sample=>sample.visible>0&&sample.ink&&sample.width>0&&sample.height>0)).toBe(true);
      const maxOffset=Math.max(...samples.map(sample=>sample.maxOffset));expect(maxOffset).toBeLessThan(1.5);
      console.log('zoom frames:',samples.length,'maximum offset (CSS px):',maxOffset);
    }
    const svg=page.getByLabel('Çizim alanı'),box=(await svg.boundingBox())!;
    const from={x:box.x+box.width*.25,y:box.y+box.height*.38},to={x:box.x+box.width*.65,y:box.y+box.height*.42};
    const clip={x:Math.max(0,from.x-20),y:Math.max(0,from.y-25),width:to.x-from.x+40,height:to.y-from.y+50};
    await page.mouse.move(from.x,from.y);await page.mouse.down();await page.mouse.move(to.x,to.y,{steps:35});await page.mouse.up();
    await expect(page.locator('.ink-page').first()).toHaveAttribute('data-ink-overlay-count','1',{timeout:20000});
    const saved=await page.waitForResponse(response=>response.url().endsWith('/ink')&&response.request().method()==='PUT',{timeout:20000});expect(saved.status()).toBe(200);
    const projection=await (await get(first,`/api/notes/${note!.id}/ink`)).json() as {pendingStrokes:unknown[]};expect(projection.pendingStrokes).toHaveLength(1);
    await expect(page.locator('.ink-page').first()).toHaveAttribute('data-ink-overlay-count','1');await page.mouse.move(1590,990);await page.waitForTimeout(300);
    const before=await page.screenshot({clip,path:testInfo.outputPath('draft.png')});
    second=await prepareStage(work,'second');
    const staged=spawnSync('python3',['-I',path.join(root,'desktop/tests/ink-tile-stage.py'),path.join(second.data,'library.sqlite3')],{input:JSON.stringify({noteId:note!.id,stroke:projection.pendingStrokes[0]}),encoding:'utf8',env:{PATH:process.env.PATH,HOME:path.join(second.home,'home'),NOTES_IMPL:'python'}});
    expect(staged.status,staged.stderr).toBe(0);await launch(second);redirected=true;
    try{await expect.poll(()=>tileRequests,{timeout:40000}).toBeGreaterThan(0);}catch(error){console.log('redirected requests:',redirectedRequests);throw error;}
    if(zoom===150){
      // A later line remains a draft while the earlier saved tile is delayed.
      await page.mouse.move(box.x+box.width*.12,box.y+box.height*.18);await page.mouse.down();
      await page.mouse.move(box.x+box.width*.24,box.y+box.height*.2,{steps:12});await page.mouse.up();
      await expect(page.locator('.ink-page').first()).toHaveAttribute('data-ink-overlay-count','2');
      await page.mouse.move(1590,990);
    }
    for(let i=0;i<5;i++){const heldFrame=await page.screenshot({clip,path:testInfo.outputPath(`held-${i}.png`)});const difference=await compare(page,before,heldFrame);expect(difference.max).toBeLessThanOrEqual(8);}
    expect(await page.locator('.ink-page').first().getAttribute('data-ink-overlay-count')).toBe(zoom===150?'2':'1');
    // Sample the actual tile canvases on every animation frame across the
    // handoff. Compare on white, as the visible page composites transparent
    // ink over its base. The later draft (if any) is already in the baseline.
    await page.evaluate(({from,to,box})=>{
      const w=window as any,element=document.querySelector<HTMLElement>('.ink-page')!;
      const sample=()=>{const bucket=Number(element.dataset.inkBucket),out=new Map<string,Uint8ClampedArray>();
        const x0=(from.x-box.x-20)/box.width*1080*bucket,x1=(to.x-box.x+20)/box.width*1080*bucket;
        const y0=(from.y-box.y-25)/box.height*1527*bucket,y1=(to.y-box.y+25)/box.height*1527*bucket;
        for(const canvas of document.querySelectorAll<HTMLCanvasElement>('.ink-tile')){
          const [column,row]=(canvas.dataset.tile??'').split(',').map(Number);
          const l=Math.max(0,Math.floor(x0-column*512)),r=Math.min(512,Math.ceil(x1-column*512));
          const t=Math.max(0,Math.floor(y0-row*512)),b=Math.min(512,Math.ceil(y1-row*512));if(l>=r||t>=b)continue;
          out.set(`${column},${row}`,canvas.getContext('2d')!.getImageData(l,t,r-l,b-t).data);
        }return out;};
      const reference=sample();if(!reference.size)throw Error('stroke region has no tiles');w.__handoffFrames=[];w.__handoffSampling=true;
      const tick=()=>{if(!w.__handoffSampling)return;const current=sample();let max=0;
        for(const [key,a] of reference){const b=current.get(key);if(!b||b.length!==a.length){max=255;break;}
          for(let i=0;i<a.length;i+=4)for(let c=0;c<3;c++)max=Math.max(max,Math.abs(Math.round((a[i+c]*a[i+3]+255*(255-a[i+3]))/255)-Math.round((b[i+c]*b[i+3]+255*(255-b[i+3]))/255)));}
        w.__handoffFrames.push(max);requestAnimationFrame(tick);};requestAnimationFrame(tick);
    },{from,to,box});
    held=false;release();await expect(page.locator('.ink-page').first()).toHaveAttribute('data-ink-overlay-count',zoom===150?'1':'0',{timeout:60000});await page.waitForTimeout(200);
    const handoffFrames=await page.evaluate(()=>{const w=window as any;w.__handoffSampling=false;return w.__handoffFrames as number[];});
    expect(handoffFrames.length).toBeGreaterThan(1);expect(Math.max(...handoffFrames)).toBeLessThanOrEqual(8);
    console.log(`handoff ${zoom}%: ${handoffFrames.length} frames, max ${Math.max(...handoffFrames)}`);
    const after=await page.screenshot({clip,path:testInfo.outputPath('saved.png')});const difference=await compare(page,before,after);
    console.log(`ink ${zoom}%:`,JSON.stringify(difference));expect(difference.max).toBeLessThanOrEqual(8);expect(errors).toEqual([]);
  }finally{held=false;release();await page.unrouteAll({behavior:'wait'});await page.close();await stop(first);await stop(second);await rm(work,{recursive:true,force:true});}
});
