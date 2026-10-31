import {test,expect} from '@playwright/test';
import {inkTileBucket,snapInkOffset,visibleInkTiles} from '../src/PageStack';

test('ink bucket never silently undersamples',()=>{
  expect([1,1.01,1.5,1.51,2,2.01,3,3.01,4].map(inkTileBucket)).toEqual([1,1.5,1.5,2,2,3,3,4,4]);
  for(const value of [0,-1,NaN,Infinity,4.01])expect(inkTileBucket(value)).toBeNull();
});

test('tile snapping includes the fractional page origin',()=>{
  const origin=100.13,dpr=1.44,offset=123.4;
  const snapped=snapInkOffset(origin,offset,dpr)!;
  expect((origin+snapped)*dpr).toBeCloseTo(Math.round((origin+offset)*dpr),10);
  expect(snapInkOffset(origin,offset,0)).toBeNull();
  expect(snapInkOffset(origin,NaN,dpr)).toBeNull();
});

test('visible ink tiles include one tile of margin and stay inside the page',()=>{
  const page={left:100,top:200,width:1200,height:1600};
  const size={width:1200,height:1600};
  expect(visibleInkTiles(page,{left:700,top:800,right:900,bottom:1000},size,2)).toEqual({columnStart:1,columnEnd:4,rowStart:1,rowEnd:4});
  expect(visibleInkTiles(page,{left:0,top:0,right:150,bottom:250},size,2)).toEqual({columnStart:0,columnEnd:1,rowStart:0,rowEnd:1});
  expect(visibleInkTiles(page,{left:0,top:0,right:100,bottom:100},size,2)).toBeNull();
  expect(visibleInkTiles(page,{left:0,top:0,right:150,bottom:250},size,1.2)).toBeNull();
  expect(visibleInkTiles(page,{left:0,top:0,right:150,bottom:250},size,2,Infinity)).toBeNull();
});
