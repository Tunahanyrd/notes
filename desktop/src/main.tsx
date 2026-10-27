import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './style.css';
import './page-style.css';
// Windows shows focus rectangles only while navigating with the keyboard. WebKit
// also treats Escape after a mouse click as keyboard focus, so track the modality.
const navigationKeys=new Set(['Tab','ArrowUp','ArrowDown','ArrowLeft','ArrowRight','Home','End','PageUp','PageDown']);
document.addEventListener('keydown',event=>{if(navigationKeys.has(event.key))document.documentElement.dataset.keyboard='1';},true);
document.addEventListener('pointerdown',()=>{delete document.documentElement.dataset.keyboard;},true);
// A desktop app, not a web page: no browser context menu outside text
// fields, no reload/view-source/devtools keys, no dragging UI images away.
const editable=(target:EventTarget|null)=>target instanceof Element&&!!target.closest('input,textarea,[contenteditable="true"]');
document.addEventListener('contextmenu',event=>{if(!editable(event.target))event.preventDefault();});
document.addEventListener('keydown',event=>{const key=event.key.toLowerCase(),mod=event.ctrlKey||event.metaKey;if(key==='f5'||key==='f12'||(mod&&(key==='r'||key==='u'))||(mod&&event.shiftKey&&['i','j','c'].includes(key)))event.preventDefault();},true);
document.addEventListener('dragstart',event=>{if(event.target instanceof HTMLImageElement)event.preventDefault();});
ReactDOM.createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);
