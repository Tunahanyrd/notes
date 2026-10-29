#!/usr/bin/env node
// README screenshots from SYNTHETIC demo data only.
//
// Regenerate (from the repository root):
//
//     cargo build -p notes-engine                      # or --release
//     cargo build --release --manifest-path tools/ink-renderer/Cargo.toml   # handwriting thumbnails
//     (cd desktop && npm ci && npm run build)          # desktop/dist
//     node tools/screenshots.mjs                       # writes docs/screenshots/{list,editor,ink,folders}.png
//
// The demo library is desktop/tests/fixtures/demo-library/library.sqlite3: an
// invented user (English titles, folders, rich text, two handwriting notes, a
// favorite) generated once by reference/python/generators/demo/gen_demo_library.py.
// The script copies it into a fresh private temporary directory, starts
// notes-engine there with --offline (no network, no account, no session) and
// with HOME/XDG_* pointed into that directory, so the real library under
// ~/.local/state is never read or written. Nothing is sent to Samsung Cloud.
//
// Options (environment):
//   NOTES_ENGINE_BIN   engine binary (default target/{release,debug}/notes-engine)
//   NOTES_FRONTEND_DIR built UI (default desktop/dist)
//   CHROMIUM           browser executable (default: /usr/bin/chromium if present,
//                      else Playwright's bundled Chromium)
//   SCREENSHOT_OUT     output directory (default docs/screenshots)
//   SCREENSHOT_SCALE   deviceScaleFactor (default 2)
// The engine runs --offline, so /api/status says "offline" and the UI would show
// its offline banner on every screen; for the pictures only, that one response is
// rewritten to a connected, idle state (account label "Demo"). Notes, thumbnails
// and page images all come from the real engine.
// PNGs are quantized with ImageMagick (`magick`) when it is installed and
// recompressed with oxipng when that is installed.
import {spawn,spawnSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {existsSync} from 'node:fs';
import {mkdtemp,mkdir,cp,chmod,readFile,rm,stat,rename} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';

const root=path.resolve(import.meta.dirname,'..');
const require=createRequire(path.join(root,'desktop/package.json'));
const {chromium}=require('@playwright/test');

const library=path.join(root,'desktop/tests/fixtures/demo-library/library.sqlite3');
const engine=process.env.NOTES_ENGINE_BIN||['release','debug'].map(kind=>path.join(root,'target',kind,'notes-engine')).find(p=>existsSync(p));
const frontend=path.resolve(process.env.NOTES_FRONTEND_DIR||path.join(root,'desktop/dist'));
const out=path.resolve(process.env.SCREENSHOT_OUT||path.join(root,'docs/screenshots'));
const scale=Number(process.env.SCREENSHOT_SCALE||2);
if(!engine)throw Error('notes-engine not found: cargo build -p notes-engine');
if(!existsSync(path.join(frontend,'index.html')))throw Error(`no built UI in ${frontend}: (cd desktop && npm run build)`);
if(!existsSync(library))throw Error(`demo library missing: ${library}`);

const work=await mkdtemp(path.join(tmpdir(),'notes-screenshots-'));await chmod(work,0o700);
const dirs=Object.fromEntries(['data','home','state','config','cache','share','run'].map(name=>[name,path.join(work,name)]));
for(const dir of Object.values(dirs))await mkdir(dir,{mode:0o700});
await cp(library,path.join(dirs.data,'library.sqlite3'));await chmod(path.join(dirs.data,'library.sqlite3'),0o600);
const runtime=path.join(work,'runtime.json');
const env={PATH:process.env.PATH,LANG:'en_US.UTF-8',HOME:dirs.home,XDG_STATE_HOME:dirs.state,XDG_CONFIG_HOME:dirs.config,
  XDG_CACHE_HOME:dirs.cache,XDG_DATA_HOME:dirs.share,XDG_RUNTIME_DIR:dirs.run,LINUX_NOTES_PROJECT_ROOT:root,TZ:'UTC'};
const child=spawn(engine,['--data-dir',dirs.data,'--offline','--no-browser','--no-auto-sync','--port','0',
  '--runtime-file',runtime,'--frontend-dir',frontend],{cwd:work,stdio:['ignore','ignore','inherit'],env});

const sleep=ms=>new Promise(r=>setTimeout(r,ms));
let browser;
try{
  let session;
  for(let i=0;i<200&&!session;i++){try{session=JSON.parse(await readFile(runtime,'utf8'));}catch{if(child.exitCode!==null)throw Error('engine exited early');await sleep(100);}}
  if(!session)throw Error('engine did not become ready');
  const executablePath=process.env.CHROMIUM||(existsSync('/usr/bin/chromium')?'/usr/bin/chromium':undefined);
  browser=await chromium.launch({executablePath,headless:true,args:['--no-sandbox','--font-render-hinting=none']});
  const context=await browser.newContext({viewport:{width:1440,height:900},deviceScaleFactor:scale,locale:'tr-TR',timezoneId:'Europe/Istanbul',colorScheme:'light'});
  const page=await context.newPage();
  const problems=[];
  page.on('pageerror',error=>problems.push('pageerror: '+error.message));
  await page.route('**/api/status',async route=>{
    const response=await route.fetch();const status=await response.json();
    status.connected=true;status.accountLabel='Demo';status.sync={...status.sync,state:'idle',lastSyncedAt:Date.parse('2026-10-27T09:35:00Z')};
    await route.fulfill({response,json:status});
  });
  await page.goto(session.url);
  await page.getByRole('button',{name:/Trip to Lisbon/}).first().waitFor();
  const settle=async()=>{
    // Handwriting thumbnails are rendered on demand (202 "pending" first).
    await page.waitForFunction(()=>![...document.querySelectorAll('.thumbnail-empty')].some(e=>e.getClientRects().length),null,{timeout:120000});
    // Wait for every visible image (thumbnails / page previews) to finish loading.
    await page.waitForFunction(()=>[...document.images].filter(img=>img.getBoundingClientRect().width>0).every(img=>img.complete&&img.naturalWidth>0),null,{timeout:60000});
    await page.mouse.move(1439,899);await sleep(600);
  };
  const shot=async name=>{await settle();await page.screenshot({path:path.join(out,name+'.png')});console.log('wrote',path.relative(root,path.join(out,name+'.png')));};
  await mkdir(out,{recursive:true});

  // (a) Notes grid with the folder sidebar (subfolder expanded).
  await page.getByRole('button',{name:'University klasörünü genişlet'}).click();
  await shot('list');

  // (b) A rich text note in the editor.
  await page.getByRole('button',{name:/Trip to Lisbon/}).first().click();
  const textTab=page.getByRole('button',{name:'Metni düzenle',exact:true});
  if(await textTab.count())await textTab.click();
  await page.getByLabel('Not içeriği').waitFor();
  await shot('editor');

  // (c) A handwriting note with the pen tools open.
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  await page.getByRole('button',{name:/Linear algebra – week 3/}).first().click();
  const pen=page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true});
  await pen.waitFor();await page.waitForFunction(()=>!document.querySelector('.tool-rail button[aria-label="Kalem"]')?.disabled,null,{timeout:30000});
  await pen.click();
  const openTools=page.getByRole('button',{name:'Araç panelini aç',exact:true});
  if(await openTools.count())await openTools.click();
  await page.getByRole('radiogroup',{name:'Kalem stili'}).waitFor();
  await shot('ink');

  // (d) Move-to-folder dialog over the list.
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  await page.getByRole('button',{name:/Groceries/}).first().click({button:'right'});
  await page.getByRole('menuitem',{name:/Taşı/}).first().click();
  await page.getByRole('listbox',{name:'Hedef klasör'}).waitFor();
  await shot('folders');

  if(problems.length)throw Error(problems.join('\n'));
}finally{
  await browser?.close();
  child.kill('SIGTERM');
  await Promise.race([new Promise(r=>child.once('exit',r)),sleep(5000)]);
  if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');
  await rm(work,{recursive:true,force:true});
}

// Optional size optimisation (lossless with oxipng; 256-colour quantization with magick).
const have=cmd=>spawnSync('sh',['-c',`command -v ${cmd}`]).status===0;
for(const name of ['list','editor','ink','folders']){
  const file=path.join(out,name+'.png');
  if(have('magick')){const tmp=file+'.tmp.png';const r=spawnSync('magick',[file,'-strip','-dither','None','-colors','256',`PNG8:${tmp}`]);if(r.status===0)await rename(tmp,file);else await rm(tmp,{force:true});}
  if(have('oxipng'))spawnSync('oxipng',['-o','4','--strip','safe','-q',file]);
  console.log(name+'.png',Math.round((await stat(file)).size/1024),'KB');
}
