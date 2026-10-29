import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Rail tools on a pen note: lasso select/move/delete, undo/redo reasons and
// truthful tooltips for tools that are not available. Synthetic data only.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');
const line=(x:number,y:number):[number,number][]=>[[x,y],[x+40,y+10],[x+80,y]];
async function fixture(page:Page,options:{erase?:boolean;pageReason?:string;pageErase?:boolean;blank?:boolean;highlighter?:boolean}={}){
  const requests:{path:string;method:string;data:any}[]=[];
  const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'',preview:''};
  const note={...base,id:'ink-tools',title:'Araç denemesi'};
  const erase=options.erase!==false;
  const saved=[['s-a',100,100],['s-b',500,500],['s-c',800,850]] as const;
  const strokes=saved.map(([id,x,y])=>({id,kind:'fountain',colorARGB:0xff2462a0,size:4,layerId:'layer-0',erasable:erase,bounds:[x-2,y-2,x+82,y+12]}));
  const pages:any[]=options.blank?[{id:'page-0',index:0,width:1000,height:1000,background:'pdf',strokes:[],ink:{appendInk:false,eraseInk:false,firstStroke:true,reason:null}}]:[{id:'page-0',index:0,width:1000,height:1000,background:null,strokes,ink:{appendInk:true,eraseInk:options.pageErase??erase,firstStroke:false,highlighter:options.highlighter??false,reason:options.pageReason??null}}];
  let pending:any[]=[];
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method(),data=method==='GET'?null:route.request().postDataJSON();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    requests.push({path,method,data});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Sentetik hesap',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,attachments:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:[{...note,body:undefined}]});
    if(path.endsWith('/thumbnail')||path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/editor'))return send({schemaVersion:1,revision:note.revision,model:null,reason:'Bilinmeyen biçimlendirme sözdizimi.',capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:false}});
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:pages.map(item=>({...item,available:true,complete:true}))});
    if(path.endsWith('/ink-geometry'))return send({schemaVersion:1,pageId:'page-0',strokes:saved.map(([id,x,y])=>({id,layerId:'layer-0',size:4,points:line(x,y)})),reason:null});
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:note.revision,pages,capabilities:note.syncState!=='synced'?{appendInk:false}:options.blank&&!pages[0].strokes.length?{appendInk:false,eraseInk:false,firstStroke:true}:{appendInk:true,eraseInk:erase,firstStroke:false,highlighter:options.highlighter??false},reason:null,rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:pending});
    if(path.endsWith('/ink')&&method==='PUT'){pending=data.strokes.map(({tool,...stroke}:any,index:number)=>({...stroke,id:`pending-${index}`,sourceStrokeId:stroke.sourceStrokeId??'',kind:tool==='highlighter'?'highlighter':'fountain'}));note.revision++;note.syncState='pending';return send({note});}
    if((path.endsWith('/ink-move')||path.endsWith('/ink-erase'))&&method==='PUT'){note.revision++;note.syncState='pending';return send({note});}
    if(method==='GET')return send({note});return send({error:{code:'unexpected_mutation',message:'Beklenmeyen değişiklik'}},422);
  });
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Araç denemesi/}).click();await expect(page.getByLabel('Çizim alanı')).toBeAttached();
  return {requests,finishFirst:()=>{const first=pending[0];pages[0].strokes=[{...first,template:true}];pages[0].ink={...pages[0].ink,appendInk:true,firstStroke:false};pending=[];note.syncState='synced';}};
}
const rail=(page:Page)=>page.locator('.tool-rail');
async function lassoAround(page:Page,cx:number,cy:number){
  const svg=page.getByLabel('Seçim alanı');await expect(svg).toHaveClass(/selecting/);const box=(await svg.boundingBox())!;
  const at=(x:number,y:number)=>[box.x+x*box.width/1000,box.y+y*box.height/1000] as const;
  const loop=[[cx-30,cy-30],[cx+120,cy-30],[cx+120,cy+50],[cx-30,cy+50],[cx-30,cy-30]];
  await page.mouse.move(...at(loop[0][0],loop[0][1]));await page.mouse.down();for(const [x,y] of loop.slice(1))await page.mouse.move(...at(x,y),{steps:3});await page.mouse.up();
  return at;
}
test('lasso selects saved strokes, drag moves them and the move autosaves as one ink-move job',async({page})=>{
  const {requests}=await fixture(page);
  await rail(page).getByRole('button',{name:'Seçim',exact:true}).click();
  const at=await lassoAround(page,500,500);await expect(page.getByTestId('ink-selection')).toBeVisible();
  await page.mouse.move(...at(540,505));await page.mouse.down();await page.mouse.move(...at(600,545),{steps:5});await page.mouse.up();
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-move-count','1');
  await expect(rail(page).getByRole('button',{name:'Geri al',exact:true})).toBeEnabled();
  await expect.poll(()=>requests.filter(r=>r.path.endsWith('/ink-move')).length,{timeout:5000}).toBe(1);
  const move=requests.find(r=>r.path.endsWith('/ink-move'))!;
  expect(move.data.expectedRevision).toBe(1);expect(move.data.move).toHaveLength(1);
  const [group]=move.data.move;expect([group.pageId,group.layerId,group.strokeIds]).toEqual(['page-0','layer-0',['s-b']]);
  expect(Number.isInteger(group.dx)&&Number.isInteger(group.dy)).toBe(true);expect(group.dx).toBeGreaterThan(40);expect(group.dy).toBeGreaterThan(25);
  // Undoing the saved move needs another Cloud move; while the first one syncs the reason is shown.
  const undo=rail(page).getByRole('button',{name:'Geri al',exact:true});await expect(undo).toBeDisabled();
  await expect(undo).toHaveAttribute('title',/Geri al: .+/);
});
test('undo right after a drag cancels the unsaved move without any Cloud request',async({page})=>{
  const {requests}=await fixture(page);
  await rail(page).getByRole('button',{name:'Seçim',exact:true}).click();
  const at=await lassoAround(page,100,100);
  await page.mouse.move(...at(140,105));await page.mouse.down();await page.mouse.move(...at(240,205),{steps:4});await page.mouse.up();
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-move-count','1');
  await rail(page).getByRole('button',{name:'Geri al',exact:true}).click();
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-move-count','0');
  const redo=rail(page).getByRole('button',{name:'Yinele',exact:true});await expect(redo).toBeEnabled();await expect(redo).toHaveAttribute('title',process.platform==='darwin'?'Yinele (⌘⇧Z)':'Yinele (Ctrl+Y)');
  await page.keyboard.press('ControlOrMeta+y');await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-move-count','1');
  await page.keyboard.press('ControlOrMeta+z');await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-move-count','0');
  await page.waitForTimeout(2200);expect(requests.filter(r=>r.method==='PUT')).toHaveLength(0);
});
test('Delete erases the lasso selection through the erase job',async({page})=>{
  const {requests}=await fixture(page);
  await rail(page).getByRole('button',{name:'Seçim',exact:true}).click();
  await lassoAround(page,800,850);await expect(page.getByTestId('ink-selection')).toBeVisible();
  await page.keyboard.press('Delete');await expect(page.getByTestId('ink-selection')).toHaveCount(0);
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-erase-count','1');
  await expect.poll(()=>requests.filter(r=>r.path.endsWith('/ink-erase')).length,{timeout:5000}).toBe(1);
  expect(requests.find(r=>r.path.endsWith('/ink-erase'))!.data.erase).toEqual([{pageId:'page-0',layerId:'layer-0',strokeIds:['s-c']}]);
  // A saved Cloud delete cannot be restored: undo says so instead of doing nothing silently.
  await expect(rail(page).getByRole('button',{name:'Geri al',exact:true})).toHaveAttribute('title',/geri getirilemiyor|eşitleniyor|Geri al: /);
});
test('unavailable tools explain the real reason, empty history is disabled with a reason',async({page})=>{
  await fixture(page,{erase:false,pageReason:'Bu sayfanın katmanı kilitli; kalem kullanılamıyor.'});
  const r=rail(page);
  await expect(r.getByRole('button',{name:'Seçim',exact:true})).toBeDisabled();
  await expect(r.getByRole('button',{name:'Seçim',exact:true})).toHaveAttribute('title','Seçim: Bu sayfanın katmanı kilitli; kalem kullanılamıyor.');
  await expect(r.getByRole('button',{name:'Silgi',exact:true})).toHaveAttribute('title',/^Silgi: Bu sayfanın katmanı kilitli/);
  await expect(r.getByRole('button',{name:'Fosforlu kalem',exact:true})).toBeDisabled();
  await expect(r.getByRole('button',{name:'Fosforlu kalem',exact:true})).toHaveAttribute('title',/^Fosforlu kalem: Bu sayfanın katmanı kilitli/);
  await expect(r.getByRole('button',{name:'Şekil',exact:true})).toHaveAttribute('title',/Şekil: /);
  await expect(r.getByRole('button',{name:'Geri al',exact:true})).toBeDisabled();
  await expect(r.getByRole('button',{name:'Geri al',exact:true})).toHaveAttribute('title','Geri al: Geri alınacak çizim işlemi yok.');
  await expect(r.getByRole('button',{name:'Yinele',exact:true})).toHaveAttribute('title','Yinele: Yinelenecek çizim işlemi yok.');
  await expect(r.getByRole('button',{name:'Kalem',exact:true})).toHaveAttribute('title',/^Kalem(: |$)/);
});
test('a stroke-free page takes a first stroke in the panel colour, saved alone without a template',async({page})=>{
  const {requests}=await fixture(page,{blank:true});
  const pen=rail(page).getByRole('button',{name:'Kalem',exact:true});await expect(pen).toBeEnabled();
  await expect(page.getByRole('complementary',{name:'Kalem araçları'}).getByLabel('Kalem kalınlığı')).toBeVisible();
  await expect(rail(page).getByRole('button',{name:'Silgi',exact:true})).toBeDisabled();
  await page.getByRole('button',{name:'Kalem rengi 1',exact:true}).click();
  await pen.click();const svg=page.getByLabel('Çizim alanı');await expect(svg).toHaveClass(/drawing/);const box=(await svg.boundingBox())!;
  await page.mouse.move(box.x+box.width*.3,box.y+box.height*.3);await page.mouse.down();await page.mouse.move(box.x+box.width*.5,box.y+box.height*.35,{steps:4});await page.mouse.up();
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-draft-count','1');
  // The first line is saved alone, but the pen stays available.
  await expect(pen).toBeEnabled();
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:5000}).toBe(1);
  const saved=requests.find(r=>r.method==='PUT'&&r.path.endsWith('/ink'))!;
  expect(saved.data.strokes).toHaveLength(1);const [stroke]=saved.data.strokes;
  expect(Object.keys(stroke).sort()).toEqual(['colorARGB','pageId','points','size']);
  expect([stroke.pageId,stroke.colorARGB,stroke.size]).toEqual(['page-0',0xffff343b,2]);
});
test('the page writer facts gate the eraser even when the note allows erasing',async({page})=>{
  await fixture(page,{pageErase:false,pageReason:'Bu sayfadaki PDF katmanında silgi henüz kullanılamıyor.'});
  const eraser=rail(page).getByRole('button',{name:'Silgi',exact:true});await expect(eraser).toBeDisabled();
  await expect(eraser).toHaveAttribute('title','Silgi: Bu sayfadaki PDF katmanında silgi henüz kullanılamıyor.');
  await expect(rail(page).getByRole('button',{name:'Kalem',exact:true})).toBeEnabled();
});
test('the highlighter draws a translucent band and is saved alone as its own tool, after pen strokes',async({page})=>{
  const {requests}=await fixture(page,{highlighter:true});
  const marker=rail(page).getByRole('button',{name:'Fosforlu kalem',exact:true});await expect(marker).toBeEnabled();
  const svg=page.getByLabel('Çizim alanı');const box=(await svg.boundingBox())!;
  const stroke=async(y:number)=>{await page.mouse.move(box.x+box.width*.3,box.y+box.height*y);await page.mouse.down();await page.mouse.move(box.x+box.width*.6,box.y+box.height*y,{steps:4});await page.mouse.up();};
  // One pen stroke, then one highlighter stroke in the second highlighter colour.
  await rail(page).getByRole('button',{name:'Kalem',exact:true}).click();await expect(svg).toHaveClass(/drawing/);await stroke(.2);
  await marker.click();await expect(marker).toHaveAttribute('aria-pressed','true');
  await expect(page.getByRole('heading',{name:'Fosforlu kalem'})).toBeVisible();
  await page.getByRole('button',{name:'Fosforlu kalem rengi 2',exact:true}).click();
  await stroke(.4);
  const band=page.locator('polyline.ink-highlighter');await expect(band).toHaveCount(1);
  await expect(band).toHaveAttribute('stroke','#37d366');expect(Number(await band.getAttribute('stroke-opacity'))).toBeCloseTo(0x73/255,3);
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:5000}).toBe(1);
  const saved=requests.find(r=>r.method==='PUT'&&r.path.endsWith('/ink'))!;
  // Pen strokes go first; the highlighter waits in the draft for its own write.
  expect(saved.data.strokes).toHaveLength(1);expect(saved.data.strokes[0].tool).toBeUndefined();
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-draft-count','1');
  await expect(band).toHaveCount(1);
});
test('a highlighter-only draft is sent as one tool:highlighter stroke with alpha 0x73',async({page})=>{
  const {requests}=await fixture(page,{highlighter:true});
  await rail(page).getByRole('button',{name:'Fosforlu kalem',exact:true}).click();
  const svg=page.getByLabel('Çizim alanı');await expect(svg).toHaveClass(/drawing/);const box=(await svg.boundingBox())!;
  await page.mouse.move(box.x+box.width*.3,box.y+box.height*.6);await page.mouse.down();await page.mouse.move(box.x+box.width*.7,box.y+box.height*.6,{steps:4});await page.mouse.up();
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:5000}).toBe(1);
  const [value]=requests.find(r=>r.method==='PUT'&&r.path.endsWith('/ink'))!.data.strokes;
  expect(Object.keys(value).sort()).toEqual(['colorARGB','pageId','points','size','tool']);
  expect([value.tool,value.pageId,value.colorARGB,value.size]).toEqual(['highlighter','page-0',0x73ffcb30,24]);
});


test('a stroke-free page accepts another line before and during the first batch',async({page})=>{
  const {requests,finishFirst}=await fixture(page,{blank:true});
  const pen=rail(page).getByRole('button',{name:'Kalem',exact:true});await pen.click();
  const svg=page.getByLabel('Çizim alanı');const box=(await svg.boundingBox())!;
  const draw=async(y:number)=>{await expect(svg).toHaveClass(/drawing/);await page.mouse.move(box.x+box.width*.3,box.y+box.height*y);await page.mouse.down();await page.mouse.move(box.x+box.width*.5,box.y+box.height*(y+.05),{steps:4});await page.mouse.up();};
  await draw(.2);await draw(.4);
  await expect(page.locator('.editor-footer')).toHaveAttribute('data-ink-draft-count','2');
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:5000}).toBe(1);
  await expect(pen).toBeEnabled();await expect(svg).toHaveClass(/drawing/);await draw(.6);
  finishFirst();
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:15000}).toBe(2);
  const writes=requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink'));
  expect(writes[0].data.strokes).toHaveLength(1);expect(writes[1].data.strokes).toHaveLength(2);
  expect(writes[1].data.strokes.every((stroke:any)=>stroke.sourceStrokeId==='pending-0')).toBe(true);
});
