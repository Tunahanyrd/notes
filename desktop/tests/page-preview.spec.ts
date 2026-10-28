import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Page images that are slow, busy, offline or impossible: the page stack keeps
// trying transient answers and never shows an error-sounding message.
// Synthetic content only; every request is answered by this mock.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==','base64');
const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Bu not çizimler içeriyor.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'',preview:''};
type Answer=number|'png';
async function setup(page:Page,pages:any[],answers:(index:number,attempt:number)=>Answer){
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
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages});
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

test('busy, offline and failed-job answers are retried until the page image arrives',async({page})=>{
  // 503 offline → 429 busy → 202 pending → 422 failed job (retry window) → image.
  const sequence:Answer[]=[503,429,202,422,'png'];
  const attempts=await setup(page,[{index:0,width:800,height:1000,available:true}],(_,attempt)=>sequence[Math.min(attempt,sequence.length-1)]);
  await expect(sheet(page,0).locator('.page-placeholder')).toHaveText('Bu sayfanın görüntüsü henüz hazır değil.');
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
  await expect(sheet(page,0).locator('.page-placeholder')).toHaveText('Bu sayfanın görüntüsü bu cihazda oluşturulamıyor.');
  await expect(sheet(page,1).locator('.page-placeholder')).toHaveText('Bu sayfadaki resim henüz indirilmedi.');
  await expect(sheet(page,2).locator('.page-placeholder')).toHaveText('Bu sayfanın görüntüsü bu cihazda oluşturulamıyor.');
  // 404 is final: one request, no retry loop. Unavailable pages are never requested.
  await page.waitForTimeout(1500);expect(attempts.get(0)).toBe(1);expect(attempts.has(1)).toBe(false);
  const box=(await sheet(page,1).boundingBox())!;expect(box.width/box.height).toBeCloseTo(2,1);
  await expect(page.getByText(/açılamadı|görüntüsü yok/)).toHaveCount(0);
});
