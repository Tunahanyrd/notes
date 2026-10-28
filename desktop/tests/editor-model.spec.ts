import {getSchema} from '@tiptap/core';
import {editorExtensions,plainPasteContent} from '../src/editorExtensions';
import {test,expect} from '@playwright/test';
import {modelToContent,contentToModel,argbToCss,type EditorModel} from '../src/editorModel';
const model:EditorModel={schemaVersion:1,text:'ç🌱 bold\r\nson\n',spans:[{kind:'bold',value:false,start:0,end:8,interval:0},{kind:'italic',value:true,start:1,end:3,interval:1},{kind:'foregroundColor',value:'#80223344',start:0,end:14,interval:2},{kind:'fontSize',value:17.5,start:10,end:13,interval:3}],paragraphs:[{index:0,alignment:'center',parsingState:false},{index:2,alignment:'right',bullet:{kind:'check',checked:false,typeList:[2,8]},indentLevel:{level:2,direction:'rtl'}}]};
test('Samsung editor bridge retains UTF-16 spans, false marks, ARGB, native intervals, CRLF and trailing empty paragraph',()=>{expect(contentToModel(modelToContent(model))).toEqual(model);expect(argbToCss('#80223344')).toBe('#22334480');});
test('Samsung editor bridge rejects unsupported anchors and surrogate-splitting ranges instead of dropping data',()=>{
  expect(()=>modelToContent({...model,spans:[{...model.spans[0],start:2,end:3}]})).toThrow();
  expect(()=>modelToContent({...model,spans:[{...model.spans[0],start:3,end:3}]})).toThrow(/konuma bağlı/);
  expect(()=>contentToModel({type:'doc',content:[{type:'heading',content:[{type:'text',text:'do not flatten'}]}]})).toThrow();
});
test('Samsung editor bridge keeps adjacent independent marks separate and carries a style through a newline',()=>{
  const sample:EditorModel={schemaVersion:1,text:'a\nb',paragraphs:[],spans:[{kind:'bold',value:true,start:0,end:1,interval:0},{kind:'bold',value:true,start:1,end:3,interval:3}]};
  expect(contentToModel(modelToContent(sample))).toEqual(sample);
});

test('native marks and paragraph attributes survive the actual ProseMirror schema',()=>{const schema=getSchema(editorExtensions);const doc=schema.nodeFromJSON(modelToContent(model));doc.check();expect(contentToModel(doc.toJSON())).toEqual(model);});

test('formatted trailing empty paragraph survives Enter and a cleared first paragraph stays writable',()=>{
  const sample:EditorModel={schemaVersion:1,text:'liste\n',spans:[],paragraphs:[{index:1,bullet:{kind:'check',checked:false},alignment:'center'}]};
  expect(contentToModel(modelToContent(sample))).toEqual(sample);
  const empty={type:'doc',content:[{type:'paragraph',attrs:{nativeAlignment:'center',nativeBullet:{kind:'check',checked:false}}}]};
  expect(contentToModel(empty)).toEqual({schemaVersion:1,text:'',spans:[],paragraphs:[]});
  expect(()=>modelToContent({...sample,text:'',paragraphs:[{index:0,alignment:'center'}]})).toThrow();
});

test('text-anchored objects (objectSpan) round-trip as atoms with their marks and positions',()=>{
  const sample:EditorModel={schemaVersion:1,text:'a\uFFFCb\n\uFFFC',spans:[{kind:'bold',value:true,start:0,end:3,interval:3}],paragraphs:[],objects:[{start:1,objectId:'0e5d4a3c-4444-4c1e-9a55-0a4fbb2f7a10',type:'image'},{start:4,objectId:'0e5d4a3c-5555-4c1e-9a55-0a4fbb2f7a10'}]};
  const content=modelToContent(sample);
  expect(content.content![0].content!.map(c=>c.type)).toEqual(['text','samsungObject','text']);
  expect(content.content![0].content![1].marks!.map(m=>m.type)).toEqual(['samsung_bold']);
  expect(contentToModel(content)).toEqual(sample);
  const schema=getSchema(editorExtensions);const doc=schema.nodeFromJSON(content);doc.check();expect(contentToModel(doc.toJSON())).toEqual(sample);
  // Anchors must sit on U+FFFC, ascending, with distinct ids.
  expect(()=>modelToContent({...sample,objects:[{...sample.objects![0],start:0}]})).toThrow();
  expect(()=>modelToContent({...sample,objects:[sample.objects![1],sample.objects![0]]})).toThrow();
  expect(()=>modelToContent({...sample,objects:[sample.objects![0],{...sample.objects![1],objectId:sample.objects![0].objectId}]})).toThrow();
  // A model without anchors keeps its exact shape (no empty objects key).
  expect(contentToModel(modelToContent({schemaVersion:1,text:'x',spans:[],paragraphs:[]}))).toEqual({schemaVersion:1,text:'x',spans:[],paragraphs:[]});
  expect(plainPasteContent('a\uFFFCb')).toEqual([{type:'paragraph',content:[{type:'text',text:'ab'}]}]);
});
