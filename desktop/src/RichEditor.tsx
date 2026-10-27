import {useEffect,useRef} from 'react';
import {EditorContent,useEditor,type Editor} from '@tiptap/react';
import {editorExtensions,plainPasteContent} from './editorExtensions';
import {modelToContent,contentToModel,type EditorModel} from './editorModel';

interface Props {model:EditorModel;editable:boolean;zoom:number;onChange:(model:EditorModel)=>void;onReady:(editor:Editor|null)=>void;onError:(message:string)=>void}
export function RichEditor({model,editable,zoom,onChange,onReady,onError}:Props){
  const latest=useRef({onChange,onError});latest.current={onChange,onError};
  const last=useRef(JSON.stringify(model));
  const editorRef=useRef<Editor|null>(null);
  const initial=useRef<ReturnType<typeof modelToContent>|null>(null);if(initial.current===null)initial.current=modelToContent(model);
  const editor=useEditor({extensions:editorExtensions,content:initial.current,editable,
    editorProps:{attributes:{class:'rich-document-body','aria-label':'Not içeriği',role:'textbox','aria-multiline':'true'},
      handlePaste:(_view,event)=>{const text=event.clipboardData?.getData('text/plain');if(text===undefined)return false;event.preventDefault();return editorRef.current?.commands.insertContent(plainPasteContent(text))??false;},
      // External rich HTML, files and unknown node types must not enter the wire model.
      handleDrop:(_view,event,_slice,moved)=>{if(moved)return false;event.preventDefault();return true;},
    },
    onUpdate:({editor:current})=>{try{const next=contentToModel(current.getJSON());last.current=JSON.stringify(next);latest.current.onChange(next);}catch(error){current.setEditable(false);latest.current.onError(error instanceof Error?error.message:'Biçimlendirme kaydedilemedi.');}},
  });
  editorRef.current=editor;
  useEffect(()=>{onReady(editor);return()=>onReady(null);},[editor,onReady]);
  useEffect(()=>{editor?.setEditable(editable,false);},[editor,editable]);
  useEffect(()=>{const serialized=JSON.stringify(model);if(editor&&serialized!==last.current){editor.commands.setContent(modelToContent(model),{emitUpdate:false});last.current=serialized;}},[editor,model]);
  return <EditorContent editor={editor} style={{zoom:zoom/100}}/>;
}
