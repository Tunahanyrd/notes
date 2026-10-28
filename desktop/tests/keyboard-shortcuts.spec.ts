import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS,formatShortcut,modifierWords,shortcutFromEvent,validateBindings,validateShortcut} from '../src/useShortcuts';
// The browser runs on the test host: macOS uses ⌘ as the primary modifier
// (stored bindings keep the portable "Ctrl+" form; display is ⌘⇧…).
// Key presses use Playwright's ControlOrMeta for the same reason.
const MAC=process.platform==='darwin';
const shown=(binding:string)=>formatShortcut(binding,MAC);
const TOKEN='synthetic_shortcut_test_session_123456';
async function setup(page:Page,options:{rich?:boolean;statusDelay?:number}={}){
  let bindings={...DEFAULT_SHORTCUTS};let statusDelay=options.statusDelay??0;let saves=0,creates=0,writes=0;let failWrite=false;let shortcutReads=0;
  const now=1777000000000;
  const notes:any[]=[];const models:Record<string,any>={};
  await page.route('**/api/**',async route=>{
    expect(route.request().headers().authorization).toBe('Bearer '+TOKEN);
    const path=new URL(route.request().url()).pathname,method=route.request().method();
    const send=(data:unknown,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(data)});
    if(path==='/api/shortcuts'){if(method==='GET')shortcutReads++;
      if(method==='PUT'){writes++;if(failWrite)return send({error:{message:'Synthetic write failure'}},500);const body=route.request().postDataJSON();expect(body.schemaVersion).toBe(2);bindings={...body.bindings};}
      return send({schemaVersion:2,bindings});
    }
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/status'){if(statusDelay){const delay=statusDelay;statusDelay=0;await new Promise(r=>setTimeout(r,delay));}return send({connected:false,accountLabel:null,loginPending:false,loginError:null,sync:{state:'offline',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,richTextEditing:!!options.rich,cloudDelete:false,cloudTrash:false,cloudFavorites:false,cloudFolders:false,attachments:false}});}
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes/editor'&&method==='POST'){creates++;const data=route.request().postDataJSON();const id='note-'+creates;models[id]=data.model;notes.unshift({id,title:data.title,body:data.model.text,preview:data.model.text,folderId:null,metadataSync:{state:'synced',error:null},createdAt:now,modifiedAt:now,revision:1,syncState:'local',editable:true,titleEditable:true,readOnlyReason:null,favorite:false,trashed:false,contentLoaded:true,source:'local',error:null});return send({note:notes[0]},201);}
    if(path==='/api/notes'){
      if(method==='POST'){creates++;const data=route.request().postDataJSON();notes.unshift({id:'note-'+creates,...data,preview:data.body,folderId:null,metadataSync:{state:'synced',error:null},createdAt:now,modifiedAt:now,revision:1,syncState:'local',editable:true,titleEditable:true,readOnlyReason:null,favorite:false,trashed:false,contentLoaded:true,source:'local',error:null});return send({note:notes[0]},201);}
      return send({notes});
    }
    const note=notes.find(n=>n.id===path.split('/')[3]);
    if(note){
      if(path.endsWith('/editor')&&method==='GET')return send(models[note.id]?{schemaVersion:1,capabilities:{textEdit:true,richStyle:true,titleEdit:true,inkEdit:false},model:models[note.id],reason:null,revision:note.revision}:{schemaVersion:1,capabilities:{textEdit:false,richStyle:false,titleEdit:true,inkEdit:false},model:null,reason:null,revision:note.revision});
      if(path.endsWith('/editor')&&method==='PUT'){saves++;const data=route.request().postDataJSON();models[note.id]=data.model;Object.assign(note,{title:data.title,body:data.model.text,revision:note.revision+1});return send({note});}
      if(method==='PUT'){saves++;Object.assign(note,route.request().postDataJSON(),{revision:note.revision+1});}
      if(path.endsWith('/title')&&method==='PATCH'){saves++;Object.assign(note,route.request().postDataJSON(),{revision:note.revision+1});}
      return send({note});
    }
    return send({error:{message:'Synthetic not found'}},404);
  });
  await page.goto('/#token='+TOKEN);
  await expect(page.getByRole('heading',{name:'Tüm notlar',exact:true})).toBeVisible();
  await expect.poll(()=>shortcutReads).toBeGreaterThan(0);await page.waitForTimeout(100);
  const settings=async()=>{await page.getByRole('button',{name:'Ayarlar',exact:true}).click();await expect(page.getByLabel('Yeni not oluştur kısayolu')).toBeEnabled();};
  return {settings,counts:()=>({saves,creates,writes}),bindings:()=>bindings,fail:()=>{failWrite=true;}};
}
const remap=async(page:Page,label:string,chord:string)=>{const input=page.getByLabel(label+' kısayolu');await input.click();await input.press(chord);};

test('shortcut validation rejects conflicts, text keys and reserved chords',()=>{
  expect(validateBindings(DEFAULT_SHORTCUTS)).toBeNull();
  expect(validateBindings({...DEFAULT_SHORTCUTS,newNote:'Ctrl+K'})).toContain('kullanılıyor');
  for(const binding of ['','N','Shift+N','Alt+F4','Meta+N','Alt+Ctrl+N','Ctrl+Q','Ctrl+C','Ctrl+Z','Ctrl+Shift+V','Ctrl+Shift+N','Ctrl+Shift+K','Ctrl+Space'])expect(validateShortcut(binding)).not.toBeNull();
  for(const binding of ['Ctrl+Shift+F','Ctrl+Shift+G','Ctrl+Shift+S','Ctrl+Alt+N','Alt+Enter','Ctrl+Plus','Ctrl+Shift+7','Ctrl+BracketLeft','Ctrl+1'])expect(validateShortcut(binding)).toBeNull();
  expect(validateBindings({...DEFAULT_SHORTCUTS,bold:'',italic:''})).toBeNull();
  expect(Object.values(DEFAULT_SHORTCUTS).filter(Boolean)).toHaveLength(new Set(Object.values(DEFAULT_SHORTCUTS)).size);
  expect(formatShortcut('Ctrl+BracketRight',false)).toBe('Ctrl+]');expect(formatShortcut('',false)).toBe('Atanmadı');
  expect(formatShortcut('Ctrl+BracketRight',true)).toBe('⌘]');expect(formatShortcut('Ctrl+Shift+F',true)).toBe('⌘⇧F');expect(formatShortcut('Alt+Enter',true)).toBe('⌥Enter');
  const key=(init:Partial<KeyboardEvent>)=>shortcutFromEvent({ctrlKey:false,shiftKey:false,altKey:false,metaKey:false,getModifierState:()=>false,key:'',code:'',...init} as any,false);
  // Linux/Windows: Meta never matches. macOS: ⌘ is the primary modifier, physical Control never matches.
  expect(key({key:'n',code:'KeyN',metaKey:true})).toBe('');
  const mac=(init:Partial<KeyboardEvent>)=>shortcutFromEvent({ctrlKey:false,shiftKey:false,altKey:false,metaKey:false,getModifierState:()=>false,key:'',code:'',...init} as any,true);
  expect(mac({key:'b',code:'KeyB',metaKey:true})).toBe('Ctrl+B');expect(mac({key:'s',code:'KeyS',metaKey:true,shiftKey:true})).toBe('Ctrl+Shift+S');
  expect(mac({key:'b',code:'KeyB',ctrlKey:true})).toBe('');expect(mac({key:'Enter',code:'Enter',altKey:true})).toBe('Alt+Enter');
  expect(key({key:'=',code:'Equal',ctrlKey:true})).toBe('Ctrl+Plus');expect(key({key:'+',code:'Equal',ctrlKey:true,shiftKey:true})).toBe('Ctrl+Plus');
  expect(key({key:'-',code:'Equal',ctrlKey:true})).toBe('Ctrl+Minus');expect(key({key:'/',code:'Digit7',ctrlKey:true,shiftKey:true})).toBe('Ctrl+Shift+7');
  expect(key({key:'ğ',code:'BracketLeft',ctrlKey:true})).toBe('Ctrl+BracketLeft');expect(key({key:'Enter',code:'Enter',altKey:true})).toBe('Alt+Enter');
});
test('remapped create and search persist after reload and original shortcut no longer creates',async({page})=>{
  const fixture=await setup(page);await fixture.settings();
  await remap(page,'Yeni not oluştur','ControlOrMeta+Shift+F');
  await remap(page,'Notlarda ara','ControlOrMeta+Shift+G');
  expect(fixture.counts().creates).toBe(0);
  await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();
  await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  expect(fixture.bindings().newNote).toBe('Ctrl+Shift+F');
  await page.reload();await fixture.settings();
  await expect(page.getByLabel('Yeni not oluştur kısayolu')).toHaveValue(shown('Ctrl+Shift+F'));
  await page.locator('.settings-page-scroll').evaluate(e=>{e.scrollTop=e.scrollHeight;});
  await page.locator('.settings-page').evaluate(e=>Promise.all(e.getAnimations().map(a=>a.finished)));
  await page.screenshot({path:'/tmp/samsung-shortcuts-synthetic.png'});
  await page.getByRole('button',{name:'Ayarları kapat'}).click();
  await page.evaluate(mac=>window.dispatchEvent(new KeyboardEvent('keydown',{key:'n',code:'KeyN',ctrlKey:!mac,metaKey:mac,bubbles:true,cancelable:true})),MAC);
  expect(fixture.counts().creates).toBe(0);
  await page.keyboard.press('ControlOrMeta+Shift+F');await expect(page.getByRole('textbox',{name:'Not başlığı'})).toBeVisible();
  expect(fixture.counts().creates).toBe(1);
  await page.keyboard.press('ControlOrMeta+Shift+G');
  await expect(page.getByRole('textbox',{name:'Notlarında ara'})).toBeFocused();
});
test('remapped save writes the actual current note and displays its shortcut',async({page})=>{
  const fixture=await setup(page);await fixture.settings();
  await remap(page,'Notu bu cihaza kaydet','ControlOrMeta+Shift+S');
  await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();
  await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  await page.getByRole('button',{name:'Ayarları kapat'}).click();
  await page.keyboard.press('ControlOrMeta+N');
  await expect(page.getByRole('textbox',{name:'Not başlığı'})).toBeEditable();
  await page.getByRole('textbox',{name:'Not başlığı'}).fill('Kısayolla kaydedilen test notu');
  await expect(page.getByRole('button',{name:'Kaydet',exact:true})).toHaveAttribute('title',new RegExp(shown('Ctrl+Shift+S').replace(/[+]/g,'\\+')));
  await page.keyboard.press('ControlOrMeta+Shift+S');
  await expect.poll(()=>fixture.counts().saves).toBe(1);
});
test('shortcut editor rejects duplicate, single letter and system keys without sending preferences',async({page})=>{
  const fixture=await setup(page);await fixture.settings();
  await remap(page,'Yeni not oluştur','ControlOrMeta+K');await expect(page.locator('.shortcut-error')).toContainText('kullanılıyor');
  await expect(page.getByLabel('Yeni not oluştur kısayolu')).toHaveValue(shown('Ctrl+N'));
  await remap(page,'Yeni not oluştur','n');await expect(page.locator('.shortcut-error')).toContainText(`${modifierWords(MAC)} ile`);
  await remap(page,'Yeni not oluştur','ControlOrMeta+Q');await expect(page.locator('.shortcut-error')).toContainText('ayrılmış');
  expect(fixture.counts()).toEqual({saves:0,creates:0,writes:0});
});
test('restore defaults is explicit, saved and persists through a reload',async({page})=>{
  const fixture=await setup(page);await fixture.settings();
  await remap(page,'Yeni not oluştur','ControlOrMeta+Shift+F');await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();
  await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  await page.getByRole('button',{name:'Varsayılanları kullan',exact:true}).click();
  expect(fixture.bindings().newNote).toBe('Ctrl+Shift+F');
  await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();
  await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  await page.reload();await fixture.settings();
  await expect(page.getByLabel('Yeni not oluştur kısayolu')).toHaveValue(shown('Ctrl+N'));
  expect(fixture.counts().writes).toBe(2);
});
test('failed write is never shown as saved and can reload authoritative settings',async({page})=>{
  const fixture=await setup(page);await fixture.settings();fixture.fail();
  await remap(page,'Yeni not oluştur','ControlOrMeta+Shift+F');await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();
  await expect(page.locator('.shortcut-error')).toContainText('kaydı doğrulanamadı');
  await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toHaveCount(0);
  await page.getByRole('button',{name:'Yeniden yükle',exact:true}).click();
  await expect(page.getByLabel('Yeni not oluştur kısayolu')).toHaveValue(shown('Ctrl+N'));
  expect(fixture.counts().writes).toBe(1);
});

test('settings list every Windows shortcut group and unassigning a binding is saved',async({page})=>{
  const fixture=await setup(page);await fixture.settings();
  for(const name of ['Genel','Not görünümü','Metin biçimlendirme','Sabit kısayollar'])await expect(page.locator('.keyboard-shortcuts').getByRole('region',{name,exact:true})).toBeVisible();
  await expect(page.getByLabel('Kalın kısayolu')).toHaveValue(shown('Ctrl+B'));await expect(page.getByLabel('Yakınlaştır kısayolu')).toHaveValue(shown('Ctrl+Plus'));
  await page.getByRole('button',{name:'Kısayolu kaldır: Notu yazdır'}).click();await expect(page.getByLabel('Notu yazdır kısayolu')).toHaveValue('Atanmadı');
  await remap(page,'Okuma modunu aç/kapat','Alt+Shift+R');
  await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  expect(fixture.bindings()).toMatchObject({printNote:'',readingMode:'Alt+Shift+R'});
});
test('Windows view shortcuts toggle reading mode, zoom, tools and print in the open note',async({page})=>{
  const fixture=await setup(page);
  await page.evaluate(()=>{(window as any).printed=0;window.print=()=>{(window as any).printed++;};});
  await page.keyboard.press('ControlOrMeta+N');await expect(page.getByRole('textbox',{name:'Not başlığı'})).toBeEditable();expect(fixture.counts().creates).toBe(1);
  const zoom=page.locator('.zoom-controls span');await expect(zoom).toHaveText('100%');
  await page.keyboard.press('ControlOrMeta+Equal');await expect(zoom).toHaveText('110%');
  await page.keyboard.press('ControlOrMeta+Minus');await page.keyboard.press('ControlOrMeta+Minus');await expect(zoom).toHaveText('90%');
  await page.locator('.canvas-scroll').hover();await page.keyboard.down('Control');await page.mouse.wheel(0,-100);await page.keyboard.up('Control');await expect(zoom).toHaveText('100%');
  await page.keyboard.press('Alt+Enter');await expect(page.locator('.editor-panel')).toHaveClass(/reading-mode/);
  await page.locator('.list-title h1').evaluate(()=>{(document.activeElement as HTMLElement|null)?.blur();});
  await page.keyboard.press('Enter');await expect(page.locator('.editor-panel')).not.toHaveClass(/reading-mode/);
  await expect(page.locator('.text-tools')).not.toHaveClass(/closed/);await page.keyboard.press('ControlOrMeta+T');await expect(page.locator('.text-tools')).toHaveClass(/closed/);
  await page.keyboard.press('ControlOrMeta+Shift+P');await expect.poll(()=>page.evaluate(()=>(window as any).printed)).toBe(1);
  expect(fixture.counts().saves).toBe(0);
});
test('macOS uses Command as the primary modifier and shows ⌘ (emulated platform)',async({page})=>{
  // The app reads navigator.platform once at start; emulate a Mac so the ⌘ path
  // is covered on Linux CI too. Physical Control is left to macOS text navigation.
  await page.addInitScript(()=>Object.defineProperty(Navigator.prototype,'platform',{get:()=>'MacIntel'}));
  const fixture=await setup(page);await fixture.settings();
  await expect(page.getByLabel('Yeni not oluştur kısayolu')).toHaveValue('⌘N');await expect(page.getByLabel('Notu yazdır kısayolu')).toHaveValue('⌘⇧P');
  await expect(page.locator('#shortcut-key-help')).toContainText('⌘ veya ⌥ ile');
  await remap(page,'Yeni not oluştur','Meta+Shift+F');await expect(page.getByLabel('Yeni not oluştur kısayolu')).toHaveValue('⌘⇧F');
  await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  expect(fixture.bindings().newNote).toBe('Ctrl+Shift+F');
  await page.getByRole('button',{name:'Ayarları kapat'}).click();
  await page.keyboard.press('Control+Shift+F');expect(fixture.counts().creates).toBe(0);
  await page.keyboard.press('Meta+Shift+F');await expect(page.getByRole('textbox',{name:'Not başlığı'})).toBeVisible();
  expect(fixture.counts().creates).toBe(1);
});
test('a new note waits for the first status before choosing plain or rich',async({page})=>{
  // Ctrl+N before the first /api/status answer used to create a plain note on a rich-capable engine.
  await setup(page,{rich:true,statusDelay:1500});
  await page.keyboard.press('ControlOrMeta+N');await expect(page.getByRole('textbox',{name:'Not içeriği'})).toHaveAttribute('contenteditable','true');
});
test('formatting shortcuts edit the rich model and follow remapping',async({page})=>{
  const fixture=await setup(page,{rich:true});
  await page.keyboard.press('ControlOrMeta+N');const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toHaveAttribute('contenteditable','true');
  await editor.click();await page.keyboard.type('Kalın satır');await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('ControlOrMeta+B');await expect(editor.locator('span[data-samsung-kind="bold"]')).toHaveText('Kalın satır');
  await page.keyboard.press('ControlOrMeta+E');await expect(editor.locator('p').first()).toHaveAttribute('style',/text-align: ?center/);
  await page.keyboard.press('ControlOrMeta+Shift+Digit8');await expect(editor.locator('p').first()).toHaveAttribute('data-native-bullet','disc');
  await page.keyboard.press('ControlOrMeta+BracketRight');await expect(editor.locator('span[data-samsung-kind="fontSize"]').first()).toHaveAttribute('style',/font-size: ?36px/);
  await page.keyboard.press('ControlOrMeta+M');await expect(editor.locator('p').first()).toHaveAttribute('style',/margin-inline-start: ?1.5em/);
  await page.keyboard.press('ControlOrMeta+S');await expect.poll(()=>fixture.counts().saves).toBe(1);
  await page.getByRole('button',{name:'Not listesine dön'}).click();await fixture.settings();
  await remap(page,'İtalik','ControlOrMeta+Shift+Y');await page.getByRole('button',{name:'Kısayolları kaydet',exact:true}).click();await expect(page.getByText('Kısayollar bu cihaza kaydedildi.')).toBeVisible();
  await page.getByRole('button',{name:'Ayarları kapat'}).click();await page.getByRole('button',{name:/Kalın satır|Metin notu/}).first().click();
  // Focus without a pointer click: Chrome reports a click's caret asynchronously
  // (selectionchange), and when that lands after Ctrl+A it collapses the selection.
  await editor.focus();await page.keyboard.press('ControlOrMeta+a');await expect.poll(()=>page.evaluate(()=>String(getSelection()))).toBe('Kalın satır');
  await page.keyboard.press('ControlOrMeta+I');await expect(editor.locator('span[data-samsung-kind="italic"]')).toHaveCount(0);
  await page.keyboard.press('ControlOrMeta+Shift+Y');await expect(editor.locator('span[data-samsung-kind="italic"]')).toHaveText('Kalın satır');
});
