import type {Editor} from '@tiptap/react';
import {freshMark} from './editorExtensions';
import {markName,type SpanKind} from './editorModel';

/** Keyboard formatting commands; the same model edits the text panel's buttons make. */
export type FormatCommand='bold'|'italic'|'underline'|'strikethrough'|'alignLeft'|'alignCenter'|'alignRight'|'numberedList'|'bulletList'|'checklist'|'fontSmaller'|'fontLarger'|'indent'|'outdent';
export const FONT_SIZES=[8,10,12,14,15,16,17,18,20,24,28,32,36,48,64] as const;
// Unstyled body text renders at 32px (style.css .rich-document-body).
const BASE_SIZE=32;
const toggle=(editor:Editor,kind:SpanKind)=>editor.isActive(markName(kind),{value:true})?editor.chain().focus().unsetMark(markName(kind)).run():editor.chain().focus().setMark(markName(kind),freshMark(true)).run();
function list(editor:Editor,kind:'disc'|'number'|'check'){
  const remove=editor.getAttributes('paragraph').nativeBullet?.kind===kind;let number=0;
  return editor.chain().focus().command(({tr})=>{
    tr.doc.nodesBetween(tr.selection.from,tr.selection.to,(node,pos)=>{if(node.type.name!=='paragraph')return;const nativeBullet=remove?{kind:'none',number:0}:kind==='check'?{kind,checked:false}:{kind,number:kind==='number'?++number:0};tr.setNodeMarkup(pos,undefined,{...node.attrs,nativeBullet});});return true;
  }).run();
}
function indent(editor:Editor,delta:number){
  const current=editor.getAttributes('paragraph').nativeIndent;
  return editor.chain().focus().updateAttributes('paragraph',{nativeIndent:{level:Math.max(0,Math.min(32,(current?.level||0)+delta)),direction:current?.direction==='rtl'?'rtl':'ltr'}}).run();
}
function fontStep(editor:Editor,direction:1|-1){
  const value=Number(editor.getAttributes(markName('fontSize')).value)||BASE_SIZE;
  const next=direction>0?FONT_SIZES.find(size=>size>value):[...FONT_SIZES].reverse().find(size=>size<value);
  if(!next)return false;
  return editor.chain().focus().setMark(markName('fontSize'),freshMark(next)).run();
}
const paragraphText=(editor:Editor)=>editor.state.selection.$from.parent.content.size>0;
export function runFormat(editor:Editor,command:FormatCommand):boolean {
  switch(command){
    case 'bold':case 'italic':case 'underline':case 'strikethrough':return toggle(editor,command);
    case 'alignLeft':case 'alignCenter':case 'alignRight':
      if(!paragraphText(editor))return false;
      return editor.chain().focus().updateAttributes('paragraph',{nativeAlignment:command==='alignLeft'?'left':command==='alignCenter'?'center':'right'}).run();
    case 'numberedList':return paragraphText(editor)&&list(editor,'number');
    case 'bulletList':return paragraphText(editor)&&list(editor,'disc');
    case 'checklist':return paragraphText(editor)&&list(editor,'check');
    case 'fontSmaller':return fontStep(editor,-1);
    case 'fontLarger':return fontStep(editor,1);
    case 'indent':return indent(editor,1);
    case 'outdent':return indent(editor,-1);
  }
}
