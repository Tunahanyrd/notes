import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Synthetic pen note with a body-flow area and a text-anchored object.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGklEQVR4nGP48+fPf0oww6gBowaMGjBcDAAAD+rzLs1qr5kAAAAASUVORK5CYII=','base64');
const TRANSPARENT=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAEklEQVR4nGNgGAWjYBSMAggAAAQQAAFVN1rQAAAAAElFTkSuQmCC','base64');
async function fixture(page:Page,options:{fulltextBarrier?:Promise<void>;multiPage?:boolean;notextUnavailable?:boolean}={}){
  const requests:{path:string;method:string;data:any;query:string}[]=[];
  const note:any={id:'pen-text',title:'Kalem ve metin',folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'Kalem \uFFFC metni',preview:''};
  let model:any={schemaVersion:1,text:'Kalem \uFFFC metni',spans:[],paragraphs:[],objects:[{start:6,objectId:'0e5d4a3c-4444-4c1e-9a55-0a4fbb2f7a10',type:'image'}]};
  const pages=[{id:'page-0',index:0,width:1000,height:1000,strokes:[{id:'source-0',kind:'fountain',colorARGB:0xff2462a0,size:4,points:[{x:10,y:20,pressure:.5,timeMs:0},{x:30,y:40,pressure:.6,timeMs:10}]}]}];
  if(options.multiPage)pages.push({id:'page-1',index:1,width:1000,height:1000,strokes:[]});
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method(),data=method==='GET'?null:route.request().postDataJSON();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    requests.push({path,method,data,query:url.search});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Sentetik hesap',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,richTextEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,attachments:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:[{...note,body:undefined}]});
    if(path.endsWith('/thumbnail')||path.endsWith('/preview')){
      if(url.searchParams.get('variant')==='notext'&&options.notextUnavailable)return send({error:{code:'preview_unavailable',message:'unavailable'}},404);
      if(url.searchParams.get('variant')==='fulltext'&&options.fulltextBarrier)await options.fulltextBarrier;
      if(url.searchParams.get('variant')==='notextbase'||url.searchParams.get('layer')==='textink')return route.fulfill({contentType:'image/png',body:url.searchParams.has('layer')?TRANSPARENT:PNG,headers:{'x-ink-tiles':'1','x-ink-snapshot':'a'.repeat(64)}});
      return route.fulfill({contentType:'image/png',body:PNG});
    }
    if(path.endsWith('/editor')&&method==='GET')return send({schemaVersion:1,revision:note.revision,model,bodyTextAreas:[{pageIndex:0,rect:[100,80,900,920],pageWidth:1000,pageHeight:1000,backgroundArgb:0xffffffff}],reason:null,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false}});
    if(path.endsWith('/editor')&&method==='PUT'){expect(data.expectedRevision).toBe(note.revision);model=data.model;Object.assign(note,{body:model.text,revision:note.revision+1});return send({note});}
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:pages.map(item=>({...item,available:true,complete:true}))});
    if(path.endsWith('/ink'))return send({schemaVersion:1,revision:note.revision,pages,capabilities:{appendInk:true},reason:null,rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:[]});
    if(path.endsWith('/shapes'))return send({schemaVersion:1,revision:note.revision,pages:[]});
    if(method==='GET')return send({note});
    return send({error:{code:'unexpected_mutation',message:'Beklenmeyen değişiklik'}},422);
  });
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Kalem ve metin/}).click();
  return {requests};
}
for(const zoom of [100,150])test(`inline editor uses page geometry at ${zoom}%`,async({page})=>{
  const {requests}=await fixture(page);
  await expect(page.getByRole('group',{name:'Görünüm'})).toHaveCount(0);
  await expect(page.getByRole('button',{name:'Metni düzenle'})).toHaveCount(0);
  await expect(page.getByLabel('Çizim alanı')).toBeAttached();
  for(let i=100;i<zoom;i+=10)await page.getByRole('button',{name:'Yakınlaştır'}).click();
  const sheet=page.locator('.page-sheet').first();
  if(zoom===100)await page.locator('.tool-rail').getByRole('button',{name:'Metin modu'}).click();
  else await sheet.dblclick({position:{x:150*zoom/100,y:100*zoom/100}});
  const editor=page.getByRole('textbox',{name:'Not içeriği'});
  await expect(editor).toHaveAttribute('contenteditable','true');
  await expect.poll(()=>requests.some(r=>r.path.endsWith('/preview')&&r.query.includes('variant=notext'))).toBe(true);
  expect(requests.some(r=>r.query.includes('variant=notextbase'))).toBe(true);
  expect(requests.some(r=>r.query.includes('layer=textink'))).toBe(true);
  const paper=(await sheet.boundingBox())!,overlay=(await page.locator('.inline-text-overlay').boundingBox())!;
  expect(Math.abs(overlay.x-paper.x-paper.width*.1)).toBeLessThanOrEqual(4);
  expect(Math.abs(overlay.y-paper.y-paper.height*.08)).toBeLessThanOrEqual(4);
  expect(Math.abs(overlay.width-paper.width*.8)).toBeLessThanOrEqual(4);
  await page.keyboard.press('Escape');await expect(editor).toHaveCount(0);
});

test('typing and bold save the anchored model; the pen still draws',async({page})=>{
  const {requests}=await fixture(page);
  await page.locator('.tool-rail').getByRole('button',{name:'Metin modu'}).click();
  const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toBeVisible();
  await editor.click();await page.keyboard.press('End');await page.keyboard.type(' ek');
  await page.keyboard.press('Shift+Control+ArrowLeft');
  await page.getByRole('button',{name:'Kalın',exact:true}).click();
  await page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true}).click();
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/editor')).length).toBe(1);
  const put=requests.find(r=>r.method==='PUT'&&r.path.endsWith('/editor'))!;
  expect(put.data.model.text).toBe('Kalem \uFFFC metni ek');
  expect(put.data.model.objects).toEqual([{start:6,objectId:'0e5d4a3c-4444-4c1e-9a55-0a4fbb2f7a10',type:'image'}]);
  expect(put.data.model.spans.some((span:any)=>span.kind==='bold'&&span.value===true)).toBe(true);
  await expect(editor).toHaveCount(0,{timeout:10000});
  expect(requests.some(r=>r.path.endsWith('/preview')&&r.query.includes('variant=fulltext'))).toBe(true);
  const svg=page.getByLabel('Çizim alanı');await expect(svg).toHaveClass(/drawing/);
  const box=(await svg.boundingBox())!;await page.mouse.move(box.x+box.width*.2,box.y+box.height*.2);await page.mouse.down();await page.mouse.move(box.x+box.width*.4,box.y+box.height*.3,{steps:4});await page.mouse.up();
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-draft-count','1');
});

test('the edited text stays visible until the new page image is decoded',async({page})=>{
  let release!:()=>void;const barrier=new Promise<void>(resolve=>{release=resolve;});
  const {requests}=await fixture(page,{fulltextBarrier:barrier});
  await page.locator('.tool-rail').getByRole('button',{name:'Metin modu'}).click();
  const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toBeVisible();
  await editor.click();await page.keyboard.press('End');await page.keyboard.type(' yenisi');await page.keyboard.press('Escape');
  await expect.poll(()=>requests.some(r=>r.path.endsWith('/preview')&&r.query.includes('variant=fulltext')),{timeout:10000}).toBe(true);
  await expect(editor).toBeVisible();await expect(editor).toContainText('yenisi');
  release();await expect(editor).toHaveCount(0);
});

test('the editor grows across the page gap and fetches the next textless page',async({page})=>{
  const {requests}=await fixture(page,{multiPage:true});
  await page.locator('.tool-rail').getByRole('button',{name:'Metin modu'}).click();
  const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toBeVisible();
  await editor.click();await page.keyboard.press('End');
  await page.keyboard.insertText(('\nSynthetic flow line').repeat(30));
  const sheet=await page.locator('.page-sheet').first().boundingBox();
  await expect.poll(async()=>((await page.locator('.inline-text-overlay').boundingBox())?.height??0)).toBeGreaterThan(sheet!.height);
  await expect.poll(()=>requests.some(r=>r.path.endsWith('/preview')&&r.query.includes('variant=notext')&&r.query.includes('page=1'))).toBe(true);
});

test('an unavailable textless image leaves the page and pen usable',async({page})=>{
  const {requests}=await fixture(page,{notextUnavailable:true});
  await page.locator('.tool-rail').getByRole('button',{name:'Metin modu'}).click();
  await expect.poll(()=>requests.some(r=>r.query.includes('variant=notext'))).toBe(true);
  await expect(page.getByRole('textbox',{name:'Not içeriği'})).toHaveCount(0);
  const pen=page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true});await expect(pen).toBeEnabled();await pen.click();
  await expect(page.getByLabel('Çizim alanı')).toHaveClass(/drawing/);
});
