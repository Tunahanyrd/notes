import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');
async function fixture(page:Page,options:{complete?:boolean;append?:boolean;failSave?:boolean;styles?:boolean;control?:{hold:boolean}}={}){
  const requests:{path:string;method:string;data:any}[]=[];let pending:any[]=[];
  const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'',preview:''};
  const notes=[{...base,id:'ink-one',title:'Çizim denemesi'},{...base,id:'ink-two',title:'İkinci çizim'}];
  const pages=[0,1].map(index=>({id:`page-${index}`,index,width:1000,height:1000,strokes:[{id:`source-${index}`,kind:'fountain',colorARGB:0xff2462a0,size:4,points:[{x:10,y:20,pressure:.5,timeMs:0},{x:30,y:40,pressure:.6,timeMs:10}]}]}));
  if(options.styles)pages[0].strokes.push({id:'source-red',kind:'fountain',colorARGB:0xffd03030,size:8,points:[{x:50,y:60,pressure:.5,timeMs:0},{x:70,y:80,pressure:.5,timeMs:10}]},{id:'source-blue-copy',kind:'fountain',colorARGB:0xff2462a0,size:4,points:[{x:90,y:90,pressure:.5,timeMs:0},{x:95,y:95,pressure:.5,timeMs:10}]});
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  const deviceDrafts=new Map<string,any>();
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method(),data=method==='GET'?null:route.request().postDataJSON();
    if(path.endsWith('/ink-draft')){const id=path.split('/')[3];if(method==='PUT'){if(data.strokes.length)deviceDrafts.set(id,data);else deviceDrafts.delete(id);}return route.fulfill({contentType:'application/json',body:JSON.stringify(deviceDrafts.get(id)??{revision:null,strokes:[]})});}
    requests.push({path,method,data});expect(route.request().headers().authorization).toBe(`Bearer ${TOKEN}`);
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Sentetik hesap',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:pending.length?1:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,attachments:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:notes.map(note=>({...note,body:undefined}))});
    const note=notes.find(item=>item.id===path.split('/')[3]);if(!note)return send({error:{code:'not_found',message:'Bulunamadı'}},404);
    if(path.endsWith('/thumbnail'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/editor'))return send({schemaVersion:1,revision:note.revision,model:null,reason:'Bilinmeyen biçimlendirme sözdizimi.',capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:false}});
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:pages.map(item=>({...item,available:true,complete:options.complete!==false}))});
    if(path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:note.revision,pages,capabilities:{appendInk:options.append!==false&&note.syncState==='synced'},reason:null,rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:note.id==='ink-one'?pending:[]});
    if(path.endsWith('/ink')&&method==='PUT'){
      if(options.failSave||options.control?.hold)return send({error:{code:'revision_conflict',message:'Sürüm değişti'}},409);
      expect(data.expectedRevision).toBe(note.revision);pending=data.strokes.map((stroke:any,index:number)=>({...stroke,id:`pending-${index}`,kind:'fountain',size:4,colorARGB:0xff2462a0}));note.revision++;note.syncState='pending';return send({note});
    }
    if(method==='GET')return send({note});return send({error:{code:'unexpected_mutation',message:'Beklenmeyen değişiklik'}},422);
  });
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Çizim denemesi/}).click();await expect(page.getByLabel('Çizim alanı')).toBeAttached();
  return {requests,notes,pages,deviceDrafts};
}
async function draw(page:Page){
  // Pen mode is shared by all pages; switch it on only when it is off.
  const pen=page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true});if(await pen.getAttribute('aria-pressed')!=='true')await pen.click();const svg=page.getByLabel('Çizim alanı');await expect(svg).toHaveClass(/drawing/);const box=(await svg.boundingBox())!;
  await page.mouse.move(box.x+box.width*.25,box.y+box.height*.2);await page.mouse.down();await page.mouse.move(box.x+box.width*.5,box.y+box.height*.3,{steps:4});await page.mouse.up();
  await expect(page.locator('.editor-footer')).toContainText('çizgi kaydedilmedi');
}
test('ink autosaves like Samsung clients, keeps page identity and lets drawing continue while the batch syncs',async({page})=>{
  const {requests}=await fixture(page);await expect(page.getByText('Sayfa ve çizim görünümü',{exact:true}).first()).toBeVisible();await expect(page.getByText('Salt okunur sayfa önizlemesi',{exact:true})).toHaveCount(0);await draw(page);await expect(page.getByLabel('Not başlığı')).toHaveAttribute('readonly','');
  await expect.poll(()=>requests.filter(request=>request.method==='PUT').length,{timeout:5000}).toBe(1);const saved=requests.find(request=>request.method==='PUT')!;
  expect(saved.path).toBe('/api/notes/ink-one/ink');expect(saved.data.expectedRevision).toBe(1);expect(saved.data.strokes.map((stroke:any)=>[stroke.pageId,stroke.sourceStrokeId])).toEqual([['page-0','source-0']]);
  for(const stroke of saved.data.strokes){expect(Object.keys(stroke).sort()).toEqual(['pageId','points','sourceStrokeId']);expect(stroke.points[0].x).toBeCloseTo(250,0);expect(stroke.points[0].y).toBeCloseTo(200,0);expect(stroke.points[0].timeMs).toBe(0);expect(stroke.points.every((point:any)=>point.pressure===.5&&Number.isInteger(point.timeMs))).toBe(true);}
  await expect(page.getByText('Çizim kaydı · Bulut bekleniyor',{exact:true})).toBeVisible();await expect(page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true})).toBeEnabled();
  await page.getByLabel('Gösterilen sayfa').selectOption('1');await draw(page);await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();
  await page.waitForTimeout(2500);expect(requests.filter(request=>request.method==='PUT')).toHaveLength(1);
  await page.getByRole('button',{name:'Not listesine dön'}).click();await page.getByRole('button',{name:/İkinci çizim/}).click();await expect(page.getByLabel('Çizim alanı')).toBeAttached();await expect(page.getByText('1 çizgi kaydedilmedi')).toHaveCount(0);
  await page.getByRole('button',{name:'Not listesine dön'}).click();await page.getByRole('button',{name:/Çizim denemesi/}).click();await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();
  await page.reload();await page.getByRole('button',{name:/Çizim denemesi/}).click();await expect(page.getByLabel('Gösterilen sayfa')).toBeVisible();await page.getByLabel('Gösterilen sayfa').selectOption('1');
  // The line drawn while the first batch was syncing survived the restart on this device.
  await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();
  // Both pages are on screen: the syncing line on page 1 and the restored draft on page 2.
  await expect(page.locator('.page-sheet[data-page-index="0"] .ink-overlay polyline')).toHaveCount(1);await expect(page.locator('.page-sheet[data-page-index="1"] .ink-overlay polyline')).toHaveCount(1);expect(requests.filter(request=>request.method==='PUT'&&request.path.endsWith('/ink'))).toHaveLength(1);
});

test('ink append stays disabled for an incomplete baseline even when the writer capability is true',async({page})=>{
  const {requests}=await fixture(page,{complete:false});await expect(page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true})).toBeDisabled();await expect(page.getByText(/Sayfanın tamamı gösterilmeden çizim açılamaz/)).toBeVisible();expect(requests.filter(request=>request.method==='PUT')).toHaveLength(0);
});
test('failed ink save preserves the actual draft, blocks metadata and allows explicit undo without retry',async({page})=>{
  const {requests}=await fixture(page,{failSave:true});await draw(page);await page.locator('.save-button').click();await expect(page.getByText('Çizimler kaydedilemedi. Taslağınız bu pencerede korunuyor.')).toBeVisible();await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();
  await page.getByRole('button',{name:'Not seçenekleri',exact:true}).click();await expect(page.locator('.note-menu').getByRole('button',{name:'Çöp kutusuna taşı'})).toBeDisabled();await page.getByRole('button',{name:'Not seçenekleri',exact:true}).click();
  await page.locator('.tool-rail').getByRole('button',{name:'Geri al',exact:true}).click();await expect(page.getByText('1 çizgi kaydedilmedi')).toHaveCount(0);await expect(page.getByLabel('Not başlığı')).not.toHaveAttribute('readonly','');expect(requests.filter(request=>request.method==='PUT')).toHaveLength(1);
});
test('unsupported writer never becomes enabled',async({page})=>{
  const {requests}=await fixture(page,{append:false});await expect(page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true})).toBeDisabled();await page.getByLabel('Çizim alanı').dispatchEvent('pointerdown',{pointerId:3,clientX:1,clientY:1,button:0});await page.getByLabel('Çizim alanı').dispatchEvent('pointercancel',{pointerId:3});await expect(page.locator('.save-button')).toBeDisabled();expect(requests.filter(request=>request.method==='PUT')).toHaveLength(0);
});

test('pointer cancellation discards only the unfinished stroke and keeps completed draft strokes',async({page})=>{
  const {requests}=await fixture(page);await page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true}).click();await expect(page.getByLabel('Çizim alanı')).toHaveClass(/drawing/);await page.locator('.tool-rail').getByRole('button',{name:'Metin modu',exact:true}).click();await expect(page.getByLabel('Çizim alanı')).not.toHaveClass(/drawing/);await draw(page);const svg=page.getByLabel('Çizim alanı'),box=(await svg.boundingBox())!;
  await page.mouse.move(box.x+80,box.y+90);await page.mouse.down();await page.mouse.move(box.x+120,box.y+130);await page.keyboard.press('Control+s');await expect(page.locator('.save-button')).toBeDisabled();
  await svg.dispatchEvent('pointercancel',{pointerId:99});await expect(page.locator('.save-button')).toBeDisabled();
  await svg.dispatchEvent('pointercancel',{pointerId:1});await page.mouse.up();await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();await expect(svg.locator('polyline')).toHaveCount(1);expect(requests.filter(request=>request.method==='PUT')).toHaveLength(0);
});

test('a remote revision whose template stroke is gone keeps the draft and blocks saving',async({page})=>{
  const control={hold:true};const {requests,notes,pages}=await fixture(page,{control});
  await draw(page);await expect.poll(()=>requests.filter(r=>r.method==='PUT').length,{timeout:5000}).toBe(1);
  control.hold=false;pages[0].strokes=[{...pages[0].strokes[0],id:'other-source'}];notes[0].revision++;
  await expect(page.getByText(/Notun sürümü değişti. Taslak çizgileriniz korunuyor/)).toBeVisible({timeout:10000});
  await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();await expect(page.locator('.save-button')).toBeDisabled();
  await page.waitForTimeout(2500);expect(requests.filter(r=>r.method==='PUT')).toHaveLength(1);
});
test('a remote revision with the same page and template moves the draft and autosaves on the new version',async({page})=>{
  const control={hold:true};const {requests,notes}=await fixture(page,{control});
  await draw(page);await expect.poll(()=>requests.filter(r=>r.method==='PUT').length,{timeout:5000}).toBe(1);
  control.hold=false;notes[0].revision++;
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.data.expectedRevision===2).length,{timeout:15000}).toBe(1);
  await expect(page.getByText('Çizim kaydı · Bulut bekleniyor')).toBeVisible();
});

test('pen panel replaces disabled text tools, chooses an existing style and redoes undone lines',async({page})=>{
  const {requests}=await fixture(page,{styles:true});
  const panel=page.getByRole('complementary',{name:'Kalem araçları'});await expect(panel).toBeVisible();
  await expect(page.getByRole('button',{name:'Kalın',exact:true})).toHaveCount(0);await expect(page.getByLabel('Yazı boyutu')).toHaveCount(0);
  const styles=panel.getByRole('radio');await expect(styles).toHaveCount(2);await expect(styles.first()).toHaveAttribute('aria-checked','true');
  await panel.getByRole('radio',{name:/#D03030, kalınlık 8/}).click();await expect(panel.getByRole('radio',{name:/#D03030/})).toHaveAttribute('aria-checked','true');
  await expect(page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true})).toHaveAttribute('aria-pressed','true');
  const svg=page.getByLabel('Çizim alanı');await expect(svg).toHaveClass(/drawing/);const box=(await svg.boundingBox())!;
  await page.mouse.move(box.x+box.width*.3,box.y+box.height*.3);await page.mouse.down();await page.mouse.move(box.x+box.width*.6,box.y+box.height*.4,{steps:4});await page.mouse.up();
  await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();
  const rail=page.locator('.tool-rail');await expect(rail.getByRole('button',{name:'Yinele',exact:true})).toBeDisabled();
  await rail.getByRole('button',{name:'Geri al',exact:true}).click();await expect(page.getByText('1 çizgi kaydedilmedi')).toHaveCount(0);
  await rail.getByRole('button',{name:'Yinele',exact:true}).click();await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();await expect(rail.getByRole('button',{name:'Yinele',exact:true})).toBeDisabled();
  await page.locator('.save-button').click();await expect(page.getByText('Çizim kaydı · Bulut bekleniyor')).toBeVisible();
  const puts=requests.filter(r=>r.method==='PUT');expect(puts).toHaveLength(1);expect(puts[0].data.strokes).toHaveLength(1);expect(puts[0].data.strokes[0].sourceStrokeId).toBe('source-red');expect(puts[0].data.strokes[0].pageId).toBe('page-0');
});
test('a new line after undo clears the redo stack',async({page})=>{
  await fixture(page);await draw(page);const rail=page.locator('.tool-rail');
  await rail.getByRole('button',{name:'Geri al',exact:true}).click();await expect(rail.getByRole('button',{name:'Yinele',exact:true})).toBeEnabled();
  const svg=page.getByLabel('Çizim alanı');const box=(await svg.boundingBox())!;
  await page.mouse.move(box.x+box.width*.4,box.y+box.height*.6);await page.mouse.down();await page.mouse.move(box.x+box.width*.5,box.y+box.height*.7,{steps:3});await page.mouse.up();
  await expect(page.getByText('1 çizgi kaydedilmedi')).toBeVisible();await expect(rail.getByRole('button',{name:'Yinele',exact:true})).toBeDisabled();
});
test('chosen colour and size travel with the new line; template style sends no override',async({page})=>{
  const {requests}=await fixture(page,{styles:true});const panel=page.getByRole('complementary',{name:'Kalem araçları'});
  await panel.getByRole('button',{name:'Kalem rengi 1',exact:true}).click();await expect(panel.getByRole('button',{name:'Kalem rengi 1',exact:true})).toHaveAttribute('aria-pressed','true');
  await panel.getByLabel('Kalem kalınlığı').fill('12');await expect(panel.getByText(/Yeni çizgi: #FF343B, kalınlık 12/)).toBeVisible();
  const svg=page.getByLabel('Çizim alanı');await page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true}).click();const box=(await svg.boundingBox())!;
  const line=async(y:number)=>{await page.mouse.move(box.x+box.width*.3,box.y+box.height*y);await page.mouse.down();await page.mouse.move(box.x+box.width*.6,box.y+box.height*(y+.05),{steps:4});await page.mouse.up();};
  await line(.3);await expect(page.locator('.ink-overlay polyline').last()).toHaveAttribute('stroke','#ff343b');await expect(page.locator('.ink-overlay polyline').last()).toHaveAttribute('stroke-width','12');
  await panel.getByRole('radio').first().click();await expect(panel.getByText(/Yeni çizgi: #2462A0, kalınlık 4/)).toBeVisible();
  await line(.6);await expect(page.getByText('2 çizgi kaydedilmedi')).toBeVisible();
  await page.locator('.save-button').click();await expect(page.getByText('Çizim kaydı · Bulut bekleniyor')).toBeVisible();
  const [put]=requests.filter(r=>r.method==='PUT');expect(put.data.strokes).toHaveLength(2);
  expect(put.data.strokes[0]).toMatchObject({sourceStrokeId:'source-0',colorARGB:0xffff343b,size:12});
  expect(Object.keys(put.data.strokes[1]).sort()).toEqual(['pageId','points','sourceStrokeId']);
});
test('leaving right after drawing and restarting keeps the line on the device and saves it on return',async({page})=>{
  const {requests,deviceDrafts}=await fixture(page);
  await draw(page);await page.getByRole('button',{name:'Not listesine dön'}).click();
  await expect.poll(()=>deviceDrafts.get('ink-one')?.strokes.length??0).toBe(1);
  await page.waitForTimeout(2000);expect(requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink'))).toHaveLength(0);
  await page.reload();await page.getByRole('button',{name:/Çizim denemesi/}).click();
  await expect(page.locator('.ink-overlay polyline')).toHaveCount(1);
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:6000}).toBe(1);
  const saved=requests.find(r=>r.method==='PUT'&&r.path.endsWith('/ink'))!;expect(saved.data.strokes).toHaveLength(1);expect(saved.data.strokes[0].pageId).toBe('page-0');
  await expect.poll(()=>deviceDrafts.has('ink-one')).toBe(false);
});
