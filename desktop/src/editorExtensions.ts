import {Extension,Mark,Node,type JSONContent} from '@tiptap/core';
import {Plugin} from '@tiptap/pm/state';
import type {Node as PMNode} from '@tiptap/pm/model';
import {Decoration,DecorationSet} from '@tiptap/pm/view';
import StarterKit from '@tiptap/starter-kit';
import {argbToCss,spanKinds,markName,bulletMarker,numberedBullets,OBJECT_CHAR,type SpanKind,type SpanValue,type HyperTextValue,type ObjectType} from './editorModel';
// style.css draws disc/number/check with ::before; other preserved native
// list kinds get an inline, non-editable marker widget below.
const cssBullets=['none','disc','number','check'];
let serial=0;
export const freshMark=(value:SpanValue)=>({value,interval:3,sourceId:`edit-${++serial}`,sourceOrder:100000+serial});
function markStyle(kind:SpanKind,value:SpanValue){
  // Detected link (URL, e-mail, phone...). Text stays editable; the stored
  // type/data are carried unchanged. No navigation from the editor.
  if(kind==='hyperText')return 'color:#1a73e8;text-decoration-line:underline';
  if(kind==='bold')return `font-weight:${value?'700':'400'}`;
  if(kind==='italic')return `font-style:${value?'italic':'normal'}`;
  if(kind==='underline')return `text-decoration-line:${value?'underline':'none'}`;
  if(kind==='strikethrough')return `text-decoration-line:${value?'line-through':'none'}`;
  // The model's number is preserved. Visual scaling is a separate view concern.
  if(kind==='fontSize')return `font-size:${Number(value)}px`;
  const original=argbToCss(String(value));
  const rgb=[3,5,7].map(offset=>parseInt(String(value).slice(offset,offset+2),16));
  const luminance=.2126*rgb[0]+.7152*rgb[1]+.0722*rgb[2];
  const adjusted=kind==='foregroundColor'&&luminance<150?rgb.map(c=>Math.round(c+(255-c)*.8)):kind==='backgroundColor'&&luminance>150?rgb.map(c=>Math.round(c*.2)):rgb;
  const dark='#'+adjusted.map(c=>c.toString(16).padStart(2,'0')).join('')+String(value).slice(1,3);
  return `${kind==='foregroundColor'?'color':'background-color'}:${original};--dark-${kind}:${dark}`;
}
function nativeMark(kind:SpanKind){return Mark.create({
  // A link must not swallow text typed after it (stored notes use interval 2).
  name:markName(kind),inclusive:kind!=='hyperText',
  addAttributes(){return {value:{default:null},interval:{default:3},sourceId:{default:null},sourceOrder:{default:100000}};},
  parseHTML(){return [{tag:`span[data-samsung-kind="${kind}"]`,getAttrs:element=>{
    try{const parsed=JSON.parse(element.getAttribute('data-samsung-attrs')||'null');return parsed&&typeof parsed==='object'?parsed:false;}catch{return false;}
  }}];},
  renderHTML({mark}){const link=kind==='hyperText'?(mark.attrs.value as HyperTextValue|null):null;return ['span',{'data-samsung-kind':kind,'data-samsung-attrs':JSON.stringify(mark.attrs),style:markStyle(kind,mark.attrs.value),...(link?{class:'native-link',title:link.data?link.data:'Bağlantı'}:{})},0];},
  // Formatting chords (Ctrl+B/I/U, ...) are user-configurable: useShortcuts + editorCommands own them.
});}
const Paragraph=Node.create({
  name:'paragraph',group:'block',content:'inline*',
  addAttributes(){return {nativeAlignment:{default:null},nativeParsingState:{default:null},nativeBullet:{default:null},nativeIndent:{default:null},separatorAfter:{default:'\n'},separatorMarks:{default:[]}};},
  parseHTML(){return [{tag:'p'},{tag:'div'}];},
  renderHTML({node}){const a=node.attrs,bullet=a.nativeBullet,indent=a.nativeIndent;const style=[a.nativeAlignment?`text-align:${a.nativeAlignment}`:'',bullet&&!cssBullets.includes(bullet.kind)?'padding-inline-start:1.6em':'',indent?`margin-inline-start:${indent.level*1.5}em`:'',indent?.direction&&indent.direction!=='none'?`direction:${indent.direction}`:''].filter(Boolean).join(';');return ['p',{style,...(bullet?{'data-native-bullet':bullet.kind,'data-native-number':bullet.number??1,'data-native-checked':String(bullet.checked??false)}:{})},0];},
  addKeyboardShortcuts(){
    const split=()=>{
      const state=this.editor.state,prior=state.selection.$from.parent,bullet=prior.attrs.nativeBullet;
      if(prior.content.size===0&&bullet&&bullet.kind!=='none')return this.editor.commands.updateAttributes('paragraph',{nativeBullet:{kind:'none',number:0}});
      const splitMarks=(state.storedMarks||state.selection.$from.marks()).map(mark=>mark.toJSON());
      return this.editor.chain().command(({tr})=>{if(!tr.selection.empty)tr.deleteSelection();return true;}).splitBlock().command(({tr})=>{
        if(tr.selection.$from.depth===0)return false;
        const current=tr.selection.$from,currentPos=current.before(),previous=tr.doc.resolve(currentPos).nodeBefore;
        if(previous?.type.name==='paragraph')tr.setNodeMarkup(currentPos-previous.nodeSize,undefined,{...previous.attrs,separatorAfter:'\n',separatorMarks:splitMarks});
        const nextBullet=bullet?.kind==='check'?{...bullet,checked:false}:numberedBullets.includes(bullet?.kind)?{...bullet,number:Math.min(2147483646,(bullet.number||0)+1)}:bullet;
        tr.setNodeMarkup(currentPos,undefined,{...current.parent.attrs,nativeBullet:nextBullet});return true;
      }).run();
    };return {'Enter':split,'Shift-Enter':split,'Mod-End':()=>this.editor.commands.setTextSelection(this.editor.state.doc.content.size-1),'Mod-Home':()=>this.editor.commands.setTextSelection(1)};
  },
});
const Checklist=Extension.create({
  name:'nativeChecklist',
  addProseMirrorPlugins(){return [new Plugin({props:{decorations(state){
    const decorations:Decoration[]=[];
    state.doc.descendants((node,pos)=>{
      if(node.type.name!=='paragraph'||!node.attrs.nativeBullet)return;
      const kind=node.attrs.nativeBullet.kind;
      if(!cssBullets.includes(kind)){const marker=bulletMarker(node.attrs.nativeBullet);decorations.push(Decoration.widget(pos+1,()=>{const span=document.createElement('span');span.className='native-bullet-marker';span.contentEditable='false';span.setAttribute('aria-hidden','true');span.style.cssText='position:absolute;inset-inline-start:0;top:0;user-select:none';span.textContent=marker;return span;},{side:-1,key:`bullet-${pos}-${marker}`,ignoreSelection:true}));return;}
      if(kind!=='check')return;
      const checked=node.attrs.nativeBullet.checked===true;
      decorations.push(Decoration.widget(pos+1,(view,getPos)=>{
        const button=document.createElement('button');button.type='button';button.className='native-check-control';button.contentEditable='false';button.disabled=!view.editable;
        button.setAttribute('role','checkbox');button.setAttribute('aria-checked',String(checked));button.setAttribute('aria-label',checked?'Yapılacak öğesini tamamlanmadı işaretle':'Yapılacak öğesini tamamla');button.textContent=checked?'☑':'☐';
        button.addEventListener('mousedown',event=>event.preventDefault());
        button.addEventListener('click',()=>{if(!view.editable)return;const at=getPos();if(at===undefined)return;const paragraph=view.state.doc.nodeAt(at-1);if(paragraph?.attrs.nativeBullet?.kind!=='check')return;view.dispatch(view.state.tr.setNodeMarkup(at-1,undefined,{...paragraph.attrs,nativeBullet:{...paragraph.attrs.nativeBullet,checked:!paragraph.attrs.nativeBullet.checked}}));});
        return button;
      },{side:-1,key:`check-${pos}-${checked}`,stopEvent:()=>true}));
    });return DecorationSet.create(state.doc,decorations);
  }},view(view){return {update(){view.dom.querySelectorAll<HTMLButtonElement>('.native-check-control').forEach(button=>{button.disabled=!view.editable;});}};}})];},
});
// An object the text anchors (objectSpan, U+FFFC): a picture inserted on a
// phone and kept in the note's floating object list. Shown as a small inline
// chip; it is not editable, not draggable and cannot be deleted here (the
// object, its note-tree entry and content file would have to go with it).
const objectLabels:Record<ObjectType,string>={image:'Resim',shape:'Şekil',line:'Çizgi',textBox:'Metin kutusu'};
const SamsungObject=Node.create({
  name:'samsungObject',group:'inline',inline:true,atom:true,selectable:false,draggable:false,
  addAttributes(){return {objectId:{default:null},objectType:{default:null}};},
  // Never created from pasted or dropped HTML: only the stored model has them.
  parseHTML(){return [];},
  renderHTML({node}){const type=node.attrs.objectType as ObjectType|null,label=(type&&objectLabels[type])||'Nesne';return ['span',{class:'native-object-anchor','data-object-type':type??'',contenteditable:'false',role:'img','aria-label':label,title:`${label}: telefonda eklendi; metindeki yeri korunur, buradan silinemez.`},label];},
  renderText(){return '';},
});
export const replaceDocumentMeta='samsungReplaceDocument';
const anchorKeys=new WeakMap<PMNode,string>();
function anchorKey(doc:PMNode){
  let key=anchorKeys.get(doc);if(key!==undefined)return key;
  const ids:string[]=[];doc.descendants(node=>{if(node.type.name==='samsungObject')ids.push(String(node.attrs.objectId));return node.isBlock;});
  key=ids.join('\n');anchorKeys.set(doc,key);return key;
}
// Edits keep every anchor, in order: a change that would delete, duplicate or
// reorder one is not applied (loading another model is marked explicitly).
const ObjectAnchors=Extension.create({
  name:'nativeObjectAnchors',
  addProseMirrorPlugins(){return [new Plugin({filterTransaction(tr,state){
    if(!tr.docChanged||tr.getMeta(replaceDocumentMeta))return true;
    const before=anchorKey(state.doc);return before===anchorKey(tr.doc);
  }})];},
});
export const editorExtensions=[StarterKit.configure({paragraph:false,bold:false,italic:false,underline:false,strike:false,blockquote:false,bulletList:false,orderedList:false,listItem:false,listKeymap:false,heading:false,code:false,codeBlock:false,hardBreak:false,horizontalRule:false,link:false,trailingNode:false}),Paragraph,Checklist,SamsungObject,ObjectAnchors,...spanKinds.map(nativeMark)];
// U+FFFC belongs to anchored objects only; pasted text never creates one.
export function plainPasteContent(text:string):JSONContent[]{return text.split(OBJECT_CHAR).join('').split(/\r\n|\r|\n/).map(line=>({type:'paragraph',content:line?[{type:'text',text:line}]:[]}));}
