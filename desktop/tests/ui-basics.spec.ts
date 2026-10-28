import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Plain UI basics: leaving a note, duplicate state, the handwriting-note flow and cursors.
// Synthetic content only; every request is answered by this mock.
const TOKEN='synthetic_local_session_1234567890';
const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:true,titleEditable:true,readOnlyReason:null,favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null};
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==','base64');
const model=(text:string)=>({schemaVersion:1,text,spans:[],paragraphs:[]});
async function setup(page:Page,{slowDuplicate=0}:{slowDuplicate?:number}={}){
  const notes:any[]=[
    {...base,id:'one',title:'',body:'Kısa bir metin.\nİkinci satır.',preview:'Kısa bir metin.'},
    {...base,id:'two',title:'Alışveriş',body:'Süt\nEkmek',preview:'Süt Ekmek'},
    {...base,id:'hand',title:'Çizim',body:'',preview:'',editable:false,titleEditable:true,readOnlyReason:'Bu not çizimler içeriyor.'},
  ];
  const requests:{method:string;path:string;data:any}[]=[];
  let created=0;
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method();
    const data=method==='GET'?null:(()=>{try{return route.request().postDataJSON();}catch{return null;}})();requests.push({method,path,data});
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:false,toolsOpen:true,defaultZoom:100});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/status')return send({connected:true,accountLabel:'Test',loginPending:false,sync:{state:'idle',lastSyncedAt:1777000000000,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,richTextEditing:true,cloudDelete:false,cloudTrash:true,cloudFavorites:true,cloudFolders:true,attachments:false}});
    if(path==='/api/notes'&&method==='GET')return send({notes:notes.filter(n=>!n.trashed).map(n=>({...n,body:undefined}))});
    if((path==='/api/notes'||path==='/api/notes/editor')&&method==='POST'){
      if(slowDuplicate)await new Promise(r=>setTimeout(r,slowDuplicate));
      const text=data.model?.text??data.body??'';const note={...base,id:`copy-${++created}`,source:'local',syncState:'local',title:data.title,body:text,preview:text.slice(0,80),...(data.model?{model:data.model}:{})};
      notes.unshift(note);return send({note},201);
    }
    const id=path.split('/')[3],note=notes.find(n=>n.id===id);if(!note)return send({error:{code:'missing',message:'Not bulunamadı.'}},404);
    if(path.endsWith('/editor')&&method==='GET'){
      if(!note.editable)return send({schemaVersion:1,capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:true},model:null,reason:'unsupported_body',revision:note.revision});
      return send({schemaVersion:1,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false},model:note.model??model(note.body),reason:null,revision:note.revision});
    }
    if(path.endsWith('/pages'))return send(note.editable?{providerAvailable:false,pages:[]}:{providerAvailable:true,pages:[0,1,2].map(index=>({index,width:800,height:1000,available:true,complete:true}))});
    if(path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/thumbnail'))return route.fulfill({status:404,contentType:'application/json',body:'{"error":{"code":"none","message":"none"}}'});
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:note.revision,reason:null,capabilities:{appendInk:false,firstStroke:true,eraseInk:false,highlighter:true},pages:[{id:'p0',index:0,width:800,height:1000,styles:[],strokes:[],drawnRect:null}],pendingStrokes:[]});
    if(path.endsWith('/ink-draft'))return send({draft:null});
    if(path.endsWith('/shapes'))return send({pages:[]});
    return send({note});
  });
  await page.goto('/#token='+TOKEN);
  await expect(page.getByRole('button',{name:/Alışveriş/})).toBeVisible();
  return {notes,requests};
}
const back=(page:Page)=>page.getByRole('button',{name:'Not listesine dön'});

test('back leaves a note the moment it opens, also while it is still loading',async({page})=>{
  await setup(page);
  await page.getByRole('button',{name:/Alışveriş/}).click();
  await back(page).click();
  await expect(page.getByRole('heading',{name:'Tüm notlar'})).toBeVisible();
  await expect(back(page)).toBeHidden();
  // Opening the same note again and leaving at once still works.
  await page.getByRole('button',{name:/Alışveriş/}).click();
  await expect(page.getByRole('textbox',{name:'Not içeriği'})).toBeVisible();
  await back(page).click();
  await expect(back(page)).toBeHidden();
});

test('a fresh duplicate is clean, opens and goes back with one click',async({page})=>{
  const {notes}=await setup(page);
  await page.getByRole('button',{name:/Metin notu/}).click();
  await expect(page.getByRole('textbox',{name:'Not içeriği'})).toHaveAttribute('contenteditable','true');
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await page.getByRole('button',{name:'Kopyasını oluştur',exact:true}).click();
  await expect.poll(()=>notes.some(n=>n.id==='copy-1')).toBe(true);
  await expect(page.getByRole('textbox',{name:'Not içeriği'})).toContainText('Kısa bir metin.');
  await expect(page.locator('.editor-footer')).not.toContainText('Kaydedilmedi');
  await expect(page.getByRole('button',{name:'Kaydet'})).toBeDisabled();
  await back(page).click();
  await expect(back(page)).toBeHidden();
  const card=page.getByRole('button',{name:/— kopya/});
  await expect(card).toBeVisible();
  await expect(card).not.toContainText('Kaydedilmedi');
  await expect(page.locator('.library-status')).not.toContainText('kaydedilmemiş');
  // Opening the duplicate and leaving right away.
  await card.click();await back(page).click();await expect(back(page)).toBeHidden();
  await card.click();await expect(page.getByRole('textbox',{name:'Not içeriği'})).toContainText('Kısa bir metin.');
  await back(page).click();await expect(back(page)).toBeHidden();
  await expect(card).not.toContainText('Kaydedilmedi');
});

test('back works while a duplicate is still being created',async({page})=>{
  await setup(page,{slowDuplicate:1500});
  await page.getByRole('button',{name:/Alışveriş/}).click();
  await expect(page.getByRole('textbox',{name:'Not içeriği'})).toHaveAttribute('contenteditable','true');
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await page.getByRole('button',{name:'Kopyasını oluştur',exact:true}).click();
  await back(page).click();
  await expect(back(page)).toBeHidden();
  // The copy lands in the list without pulling the user back into the editor.
  await expect(page.getByRole('button',{name:/Alışveriş — kopya/})).toBeVisible();
  await page.waitForTimeout(300);
  await expect(back(page)).toBeHidden();
});

test('an edit that is undone by hand does not leave the note marked as changed',async({page})=>{
  await setup(page);
  const card=page.getByRole('button',{name:/Alışveriş/});await card.click();
  const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toHaveAttribute('contenteditable','true');
  // A no-op transaction (what WebKit DOM normalisation or an IME commit produce) is not an edit.
  await editor.locator('p').last().click();await page.evaluate(()=>{const view=(document.querySelector('.ProseMirror') as any).editor.view;view.dispatch(view.state.tr.replaceWith(0,view.state.doc.content.size,view.state.doc.content));});
  await expect(page.locator('.editor-footer')).not.toContainText('Kaydedilmedi');
  // Typing and deleting the same character: once the note is read again it is clean, in the editor and on its card.
  await page.keyboard.press('End');await page.keyboard.type('!');await expect(page.locator('.editor-footer')).toContainText('Kaydedilmedi');
  await page.keyboard.press('Backspace');
  await expect(page.locator('.editor-footer')).not.toContainText('Kaydedilmedi',{timeout:9000});
  await expect(page.getByRole('button',{name:'Kaydet'})).toBeDisabled();
  await back(page).click();await expect(card).not.toContainText('Kaydedilmedi');await expect(page.locator('.library-status')).not.toContainText('kaydedilmemiş');
});

test('a handwriting note opens as its pages with the pen tools, without a text mode or jargon',async({page})=>{
  await setup(page);
  await page.getByRole('button',{name:/Çizim/}).click();
  await expect(page.locator('.page-sheet')).toHaveCount(3);
  await expect(page.getByRole('complementary',{name:'Kalem araçları'})).toBeVisible();
  await expect(page.getByRole('button',{name:/Metni göster|Sayfayı göster|Metin kopyasını/})).toHaveCount(0);
  const visible=await page.locator('.editor-panel').innerText();
  for(const jargon of [/#[0-9A-F]{6}/i,/yaklaşık geometri/,/çıkarılabilen/,/önizlemesi/,/Yalnızca metin/,/Biçimlendirme bu notta/,/Kalem türü/,/henüz düzenlenemiyor/])expect(visible).not.toMatch(jargon);
  await expect(page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true})).toHaveAttribute('title',/^Kalem(: |$)/);
});

test('cursors follow Windows: arrow on buttons, I-beam on text, hand on links, not-allowed when disabled',async({page})=>{
  await setup(page);
  const cursor=(locator:ReturnType<Page['locator']>)=>locator.evaluate(element=>getComputedStyle(element).cursor);
  expect(await cursor(page.getByRole('button',{name:'Not oluştur'}).or(page.getByRole('button',{name:'Yeni not'})).first())).toBe('default');
  expect(await cursor(page.getByRole('button',{name:/Alışveriş/}))).toBe('default');
  expect(await cursor(page.getByLabel('Notlarında ara'))).toBe('text');
  expect(await cursor(page.getByRole('button',{name:'PDF içe aktar'}))).toBe('not-allowed');
  await page.getByRole('button',{name:/Alışveriş/}).click();
  const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toHaveAttribute('contenteditable','true');
  expect(await cursor(editor)).toBe('text');
  expect(await cursor(editor.locator('p').first())).toBe('text');
  expect(await cursor(page.getByLabel('Not başlığı'))).toBe('text');
  expect(await cursor(back(page))).toBe('default');
  expect(await cursor(page.getByRole('button',{name:'Kaydet'}))).toBe('not-allowed');
  // The back arrow is a full-size target clear of the window's rounded corner.
  const box=(await back(page).boundingBox())!;expect(box.width).toBeGreaterThanOrEqual(40);expect(box.x).toBeGreaterThanOrEqual(4);
});

async function inkFixture(page:Page){
  const note={...base,id:'pen-note',title:'Kalem notu',editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',body:'',preview:''};
  const line=(x:number,y:number):[number,number][]=>[[x,y],[x+40,y+10],[x+80,y]];
  const saved=[['s-a',100,100],['s-b',300,300]] as const;
  const strokes=saved.map(([id,x,y])=>({id,kind:'fountain',colorARGB:0xff2462a0,size:4,layerId:'layer-0',erasable:true,bounds:[x-2,y-2,x+82,y+12]}));
  const pages=[{id:'page-0',index:0,width:1000,height:1000,background:null,strokes,ink:{appendInk:true,eraseInk:true,firstStroke:false,highlighter:true,reason:null}}];
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Test',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,attachments:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:[{...note,body:undefined}]});
    if(path.endsWith('/thumbnail')||path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/editor'))return send({schemaVersion:1,revision:1,model:null,reason:'unsupported',capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:false}});
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:pages.map(item=>({...item,available:true,complete:true}))});
    if(path.endsWith('/ink-geometry'))return send({schemaVersion:1,pageId:'page-0',strokes:saved.map(([id,x,y])=>({id,layerId:'layer-0',size:4,points:line(x,y)})),reason:null});
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:1,pages,capabilities:{appendInk:true,eraseInk:true,firstStroke:false,highlighter:true},reason:null,rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:[]});
    if(method==='GET')return send({note});return send({error:{code:'unexpected',message:'x'}},422);
  });
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Kalem notu/}).click();await expect(page.getByLabel('Çizim alanı')).toBeAttached();
}
test('pen, eraser, lasso and move each show their own cursor over the page',async({page})=>{
  await inkFixture(page);
  const rail=page.locator('.tool-rail');const overlay=page.locator('svg.ink-overlay').first();
  const cursor=()=>overlay.evaluate(element=>getComputedStyle(element).cursor);
  await rail.getByRole('button',{name:'Kalem',exact:true}).click();await expect(overlay).toHaveClass(/drawing/);
  expect(await cursor()).toBe('crosshair');
  await rail.getByRole('button',{name:'Silgi',exact:true}).click();await expect(overlay).toHaveClass(/erasing/);
  expect(await cursor()).toMatch(/^url\("data:image\/svg\+xml.*"\) 11 11, cell$/);
  await rail.getByRole('button',{name:'Seçim',exact:true}).click();await expect(overlay).toHaveClass(/selecting/);
  expect(await cursor()).toBe('crosshair');
  // Lasso both lines, then the selection box shows the move cursor and keeps it while dragging.
  const box=(await overlay.boundingBox())!;const at=(x:number,y:number)=>[box.x+x*box.width/1000,box.y+y*box.height/1000] as const;
  await page.mouse.move(...at(60,60));await page.mouse.down();for(const [x,y] of [[450,60],[450,360],[60,360],[60,70]])await page.mouse.move(...at(x,y),{steps:4});await page.mouse.up();
  const selection=page.getByTestId('ink-selection');await expect(selection).toBeVisible();
  expect(await selection.evaluate(element=>getComputedStyle(element).cursor)).toBe('move');
  expect(await page.evaluate(([x,y])=>(document.elementFromPoint(x,y) as Element).closest('svg')?.classList.contains('ink-overlay'),at(250,200))).toBe(true);
  await page.mouse.move(...at(250,200));await page.mouse.down();await page.mouse.move(...at(270,220),{steps:3});
  await expect(overlay).toHaveClass(/moving/);expect(await cursor()).toBe('move');
  await page.mouse.up();
});

test('dragging a note card shows grabbing, and no-drop where the drop cannot happen',async({page})=>{
  await setup(page);
  const card=page.getByRole('button',{name:/Alışveriş/});const box=(await card.boundingBox())!;
  await page.mouse.move(box.x+40,box.y+40);await page.mouse.down();await page.mouse.move(box.x+90,box.y+90,{steps:5});
  await expect(page.getByTestId('note-drag-ghost')).toBeVisible();
  expect(await page.evaluate(()=>getComputedStyle(document.body).cursor)).toBe('grabbing');
  const all=(await page.getByRole('button',{name:/^Tüm notlar/}).boundingBox())!;
  await page.mouse.move(all.x+20,all.y+all.height/2,{steps:5});
  // The note is already in "Tüm notlar" (root): still not a valid target.
  expect(await page.evaluate(()=>getComputedStyle(document.body).cursor)).toBe('no-drop');
  await page.keyboard.press('Escape');await page.mouse.up();
  expect(await page.evaluate(()=>document.documentElement.className)).not.toMatch(/note-drag/);
});
