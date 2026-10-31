import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Page images that are slow, busy, offline or impossible: the page stack keeps
// trying transient answers and never shows an error-sounding message.
// Synthetic content only; every request is answered by this mock.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==','base64');
const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Bu not çizimler içeriyor.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'',preview:''};
type Answer=number|'png';
type ManifestAnswer={status?:number;body:unknown};
async function setup(page:Page,pages:any[],answers:(index:number,attempt:number)=>Answer,options:{manifest?:(attempt:number)=>ManifestAnswer;manifestAttempts?:number[];retry?:()=>ManifestAnswer}={}){
  const attempts=new Map<number,number>();
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:false,toolsOpen:true,defaultZoom:100});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/status')return send({connected:true,accountLabel:'Test',loginPending:false,sync:{state:'idle',lastSyncedAt:1777000000000,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,richTextEditing:true,cloudDelete:false,cloudTrash:true,cloudFavorites:true,cloudFolders:true,attachments:false}});
    if(path==='/api/notes'&&method==='GET')return send({notes:[{...base,id:'paged-note',title:'Sayfalı not'}]});
    if(path.endsWith('/editor'))return send({schemaVersion:1,capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:true},model:null,reason:'unsupported_body',revision:1});
    if(path.endsWith('/pages')){
      if(method==='POST'&&options.retry){const answer=options.retry();return send(answer.body,answer.status);}
      if(options.manifest){const attempt=options.manifestAttempts?.length??0;options.manifestAttempts?.push(attempt);const answer=options.manifest(attempt);return send(answer.body,answer.status);}
      return send({providerAvailable:true,pages});
    }
    if(path.endsWith('/preview')){
      const index=Number(url.searchParams.get('page')),attempt=attempts.get(index)??0;attempts.set(index,attempt+1);
      const answer=answers(index,attempt);
      if(answer==='png')return route.fulfill({contentType:'image/png',body:PNG});
      if(answer===202)return send({state:'pending'},202);
      return send({error:{code:answer===503?'preview_offline':answer===429?'preview_busy':'preview_unavailable',message:'Sunucu iletisi'}},answer);
    }
    if(path.endsWith('/thumbnail'))return route.fulfill({status:404,contentType:'application/json',body:'{"error":{"code":"none","message":"none"}}'});
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:1,reason:'Bu notta kalem kullanılamıyor.',capabilities:{appendInk:false,firstStroke:false,eraseInk:false,highlighter:false},pages:[],pendingStrokes:[]});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    if(path.endsWith('/shapes'))return send({pages:[]});
    return send({note:{...base,id:'paged-note',title:'Sayfalı not'}});
  });
  await page.goto('/#token='+TOKEN);
  await page.getByRole('button',{name:/Sayfalı not/}).click();
  return attempts;
}
const sheet=(page:Page,index:number)=>page.locator(`.page-sheet[data-page-index="${index}"]`);

test('pending inventory stays loading and becomes a rendered page when ready',async({page})=>{
  const calls:number[]=[];
  await setup(page,[{index:0,width:800,height:1000,available:true}],()=> 'png',{manifestAttempts:calls,manifest:attempt=>({body:attempt<2?{pending:true,pages:[],providerAvailable:false}:{providerAvailable:true,pages:[{index:0,width:800,height:1000,available:true}]}})});
  await expect(page.locator('.document-loading')).toHaveCount(1);
  await expect(sheet(page,0).locator('img.page-image')).toBeVisible();
  expect(calls.length).toBe(3);
});

test('an inventory error stops polling and returns to the note content',async({page})=>{
  const calls:number[]=[];
  await setup(page,[],()=> 'png',{manifestAttempts:calls,manifest:attempt=>attempt===0?{body:{pending:true,pages:[],providerAvailable:false}}:{status:503,body:{error:{code:'preview_unavailable',message:'unavailable'}}}});
  await expect.poll(()=>calls.length).toBe(2);
  await expect(page.locator('textarea.document-body')).toBeVisible();
  expect(calls.length).toBe(2);
  await page.waitForTimeout(800);
  expect(calls.length).toBe(2);
});

test('closing a note cancels pending inventory polling',async({page})=>{
  const calls:number[]=[];
  await setup(page,[],()=> 'png',{manifestAttempts:calls,manifest:()=>({body:{pending:true,pages:[],providerAvailable:false}})});
  await expect(page.locator('.document-loading')).toHaveCount(1);
  await expect.poll(()=>calls.length).toBeGreaterThan(0);
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  const stopped=calls.length;
  await page.waitForTimeout(800);
  expect(calls.length).toBe(stopped);
});

test('busy, offline and failed-job answers are retried until the page image arrives',async({page})=>{
  // 503 offline → 429 busy → 202 pending → 422 failed job (retry window) → image.
  const sequence:Answer[]=[503,429,202,422,'png'];
  const attempts=await setup(page,[{index:0,width:800,height:1000,available:true}],(_,attempt)=>sequence[Math.min(attempt,sequence.length-1)]);
  await expect(sheet(page,0).locator('.page-placeholder')).toBeEmpty();
  await expect(sheet(page,0).locator('img.page-image')).toBeVisible({timeout:20000});
  expect(attempts.get(0)).toBe(5);
  await expect(page.getByText(/açılamadı/)).toHaveCount(0);
  await expect(sheet(page,0).locator('.page-placeholder')).toHaveCount(0);
});

test('a page with no image source gets a calm final placeholder sized by its geometry',async({page})=>{
  const attempts=await setup(page,[
    {index:0,width:800,height:1000,available:true},
    {index:1,width:1000,height:500,available:false,previewNotice:'Bu sayfadaki resim henüz indirilmedi.'},
    {index:2,width:null,height:null,available:false},
  ],index=>index===0?404:'png');
  await expect(sheet(page,0).locator('.page-placeholder')).toBeEmpty();
  await expect(sheet(page,1).locator('.page-placeholder')).toBeEmpty();
  await expect(sheet(page,2).locator('.page-placeholder')).toBeEmpty();
  // 404 is final: one request, no retry loop. Unavailable pages are never requested.
  await page.waitForTimeout(1500);expect(attempts.get(0)).toBe(1);expect(attempts.has(1)).toBe(false);
  const box=(await sheet(page,1).boundingBox())!;expect(box.width/box.height).toBeCloseTo(2,1);
  await expect(page.getByText(/açılamadı|görüntüsü yok/)).toHaveCount(0);
});

test('pages leaving the viewport stop polling and release their image',async({page})=>{
  const pages=Array.from({length:30},(_,index)=>({index,width:800,height:1000,available:true}));
  const attempts=await setup(page,pages,index=>index===0?202:'png');
  await expect.poll(()=>attempts.get(0)??0).toBeGreaterThan(0);
  await sheet(page,20).scrollIntoViewIfNeeded();
  await expect(sheet(page,20).locator('img.page-image')).toBeVisible();
  await page.waitForTimeout(500);
  const stopped=attempts.get(0);
  await page.waitForTimeout(1200);
  expect(attempts.get(0)).toBe(stopped);
  await expect(sheet(page,0).locator('img.page-image')).toHaveCount(0);
});

test('idle refresh reuses an unchanged editor projection',async({page})=>{
  let editors=0;
  page.on('request',request=>{if(new URL(request.url()).pathname.endsWith('/editor'))editors++;});
  await setup(page,[{index:0,width:800,height:1000,available:true}],()=> 'png');
  await expect(sheet(page,0).locator('img.page-image')).toBeVisible();
  await page.waitForTimeout(9000);
  expect(editors).toBe(1);
});

test('zoom keeps the same page image and does not request another preview',async({page})=>{
  const attempts=await setup(page,[{index:0,width:800,height:1000,available:true}],()=> 'png');
  const img=sheet(page,0).locator('img.page-image');await expect(img).toBeVisible();
  await img.evaluate(element=>{(window as any).__originalPageImage=element;});const src=await img.getAttribute('src');
  await page.getByRole('button',{name:'Yakınlaştır'}).click();
  await page.locator('.canvas-scroll').hover();await page.keyboard.down('Control');await page.mouse.wheel(0,-120);await page.keyboard.up('Control');
  await expect(img).toBeVisible();expect(await img.evaluate(element=>element===(window as any).__originalPageImage)).toBe(true);
  expect(await img.getAttribute('src')).toBe(src);expect(attempts.get(0)).toBe(1);
});

test('transient visibility changes during zoom keep the decoded bitmap and avoid another preview request',async({page})=>{
  await page.addInitScript(()=>{
    const Native=window.IntersectionObserver;
    window.IntersectionObserver=class extends Native {
      private callback:IntersectionObserverCallback;
      constructor(callback:IntersectionObserverCallback,options?:IntersectionObserverInit){super(callback,options);this.callback=callback;}
      observe(element:Element){if(element.matches('.page-sheet[data-page-index="0"]'))(window as any).__pageVisibility=(visible:boolean)=>this.callback([{isIntersecting:visible} as IntersectionObserverEntry],this);super.observe(element);}
    };
  });
  const attempts=await setup(page,[{index:0,width:800,height:1000,available:true}],()=> 'png');
  const img=sheet(page,0).locator('img.page-image');await expect(img).toBeVisible();
  await img.evaluate(element=>{(window as any).__zoomBitmap=element;});
  await page.evaluate(()=>(window as any).__pageVisibility(false));
  await page.waitForTimeout(100);
  await page.getByRole('button',{name:'Yakınlaştır'}).click();
  await page.evaluate(()=>(window as any).__pageVisibility(true));
  await expect(img).toBeVisible();expect(await img.evaluate(element=>element===(window as any).__zoomBitmap)).toBe(true);
  await page.waitForTimeout(1000);expect(attempts.get(0)).toBe(1);
});


test('failed inventory waits for explicit retry, then returns to pending and ready',async({page})=>{
  const calls:number[]=[];let retries=0;
  await setup(page,[{index:0,width:1,height:1,available:true}],()=> 'png',{
    manifestAttempts:calls,
    manifest:attempt=>attempt===0?{status:503,body:{error:{code:'preview_inventory_failed',message:'Sayfa bilgileri hazırlanamadı.'}}}:attempt===1?{body:{pending:true,pages:[],providerAvailable:false}}:{body:{pages:[{index:0,width:1,height:1,available:true}],providerAvailable:true}},
    retry:()=>{retries++;return {body:{pending:true,pages:[],providerAvailable:false}};},
  });
  const button=page.getByRole('button',{name:'Tekrar dene',exact:true});
  await expect(button).toBeVisible();
  await page.waitForTimeout(600);
  expect(calls).toHaveLength(1);expect(retries).toBe(0);
  await button.click();
  await expect(sheet(page,0)).toBeVisible();
  await expect.poll(()=>retries).toBe(1);
  await expect.poll(()=>calls.length).toBeGreaterThanOrEqual(3);
  await expect(button).toHaveCount(0);
});
