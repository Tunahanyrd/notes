import {useEffect,useRef} from 'react';
import {EditorContent,useEditor,type Editor} from '@tiptap/react';
import {editorExtensions,plainPasteContent,replaceDocumentMeta} from './editorExtensions';
import {modelToContent,contentToModel,type EditorModel} from './editorModel';

interface Props {model:EditorModel;editable:boolean;zoom:number;onChange:(model:EditorModel)=>void;onReady:(editor:Editor|null)=>void;onError:(message:string)=>void}
export function RichEditor({model,editable,zoom,onChange,onReady,onError}:Props){
  const latest=useRef({onChange,onError});latest.current={onChange,onError};
  const last=useRef(JSON.stringify(model));
  const editorRef=useRef<Editor|null>(null);
  // The editor's own model of what is on screen. A transaction that leaves it
  // unchanged (WebKit DOM normalisation, focus, IME commit) is not an edit and
  // must not mark the note as changed.
  const shown=useRef<string|null>(null);
  const snapshot=(current:Editor)=>{try{return JSON.stringify(contentToModel(current.getJSON()));}catch{return null;}};
  const initial=useRef<ReturnType<typeof modelToContent>|null>(null);if(initial.current===null)initial.current=modelToContent(model);
  const editor=useEditor({extensions:editorExtensions,content:initial.current,editable,
    editorProps:{attributes:{class:'rich-document-body','aria-label':'Not içeriği',role:'textbox','aria-multiline':'true'},
      handlePaste:(_view,event)=>{const text=event.clipboardData?.getData('text/plain');if(text===undefined)return false;event.preventDefault();return editorRef.current?.commands.insertContent(plainPasteContent(text))??false;},
      // External rich HTML, files and unknown node types must not enter the wire model.
      // A moved selection may not carry a text-anchored object along.
      handleDrop:(_view,event,slice,moved)=>{let anchored=false;slice.content.descendants(node=>{if(node.type.name==='samsungObject')anchored=true;});if(moved&&!anchored)return false;event.preventDefault();return true;},
    },
    onCreate:({editor:current})=>{shown.current=snapshot(current);},
    onUpdate:({editor:current})=>{try{const next=contentToModel(current.getJSON());const serialized=JSON.stringify(next);if(serialized===shown.current)return;shown.current=serialized;last.current=serialized;latest.current.onChange(next);}catch(error){current.setEditable(false);latest.current.onError(error instanceof Error?error.message:'Biçimlendirme kaydedilemedi.');}},
  });
  editorRef.current=editor;
  useEffect(()=>{onReady(editor);return()=>onReady(null);},[editor,onReady]);
  useEffect(()=>{editor?.setEditable(editable,false);},[editor,editable]);
  useEffect(()=>{const serialized=JSON.stringify(model);if(editor&&serialized!==last.current){editor.chain().setMeta(replaceDocumentMeta,true).setContent(modelToContent(model),{emitUpdate:false}).run();last.current=serialized;shown.current=snapshot(editor);}},[editor,model]);
  return <EditorContent editor={editor} style={{zoom:zoom/100}}/>;
}
