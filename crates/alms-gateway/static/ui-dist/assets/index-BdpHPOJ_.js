const __vite__mapDeps=(i,m=__vite__mapDeps,d=(m.f||(m.f=["assets/chat-actions-UbdvPXnD.js","assets/rolldown-runtime-DK3Fl9T5.js","assets/deps-DqdW5C85.js","assets/runs-D_HMln2i.js","assets/select-generation-DvILpFQd.js","assets/agents-Dxvrmzg8.js"])))=>i.map(i=>d[i]);
import{t as e}from"./rolldown-runtime-DK3Fl9T5.js";import{a as t,c as n,d as r,f as i,i as a,l as o,n as s,o as c,p as l,r as u,s as d,t as f,u as p}from"./deps-DqdW5C85.js";import{A as m,C as h,D as g,E as _,O as v,S as y,T as b,_ as x,a as ee,b as S,c as te,d as ne,f as re,g as C,h as ie,i as ae,j as oe,k as se,l as ce,m as le,n as ue,o as de,p as w,r as T,s as fe,t as pe,u as me,v as E,w as he,x as ge,y as D}from"./composer-storage-DEyjuEsp.js";import{n as O,r as k,t as A}from"./agents-Dxvrmzg8.js";import{i as j,n as _e,o as ve,r as ye,t as M}from"./runs-D_HMln2i.js";import{a as N,c as P,i as be,o as xe,r as Se,s as F,t as I}from"./chat-actions-UbdvPXnD.js";import{n as Ce,t as we}from"./select-generation-DvILpFQd.js";(function(){let e=document.createElement(`link`).relList;if(e&&e.supports&&e.supports(`modulepreload`))return;for(let e of document.querySelectorAll(`link[rel="modulepreload"]`))n(e);new MutationObserver(e=>{for(let t of e)if(t.type===`childList`)for(let e of t.addedNodes)e.tagName===`LINK`&&e.rel===`modulepreload`&&n(e)}).observe(document,{childList:!0,subtree:!0});function t(e){let t={};return e.integrity&&(t.integrity=e.integrity),e.referrerPolicy&&(t.referrerPolicy=e.referrerPolicy),e.crossOrigin===`use-credentials`?t.credentials=`include`:e.crossOrigin===`anonymous`?t.credentials=`omit`:t.credentials=`same-origin`,t}function n(e){if(e.ep)return;e.ep=!0;let n=t(e);fetch(e.href,n)}})();var Te=0;Array.isArray;function Ee(e,t,n,r,i,a){t||={};var o,s,c=t;if(`ref`in c)for(s in c={},t)s==`ref`?o=t[s]:c[s]=t[s];var u={type:e,props:c,key:n,ref:o,__k:null,__:null,__b:0,__e:null,__c:null,constructor:void 0,__v:--Te,__i:-1,__u:0,__source:i,__self:a};if(typeof e==`function`&&(o=e.defaultProps))for(s in o)c[s]===void 0&&(c[s]=o[s]);return l.vnode&&l.vnode(u),u}function De({message:e}){return Ee(`div`,{class:`contract-error-banner`,role:`alert`,children:[Ee(`strong`,{children:`Live data rejected`}),Ee(`span`,{children:e})]})}var Oe=null;function ke(e){(Oe===null||!Oe.isConnected)&&(Oe=document.createElement(`div`),Oe.dataset.almsContractBoundary=`true`,document.body.prepend(Oe)),i(Ee(De,{message:e}),Oe)}var Ae;function L(e,t,n){function r(n,r){if(n._zod||Object.defineProperty(n,"_zod",{value:{def:r,constr:o,traits:new Set},enumerable:!1}),n._zod.traits.has(e))return;n._zod.traits.add(e),t(n,r);let i=o.prototype,a=Object.keys(i);for(let e=0;e<a.length;e++){let t=a[e];t in n||(n[t]=i[t].bind(n))}}let i=n?.Parent??Object;class a extends i{}Object.defineProperty(a,"name",{value:e});function o(e){var t;let i=n?.Parent?new a:this;r(i,e),(t=i._zod).deferred??(t.deferred=[]);for(let e of i._zod.deferred)e();return i}return Object.defineProperty(o,"init",{value:r}),Object.defineProperty(o,Symbol.hasInstance,{value:t=>n?.Parent&&t instanceof n.Parent?!0:t?._zod?.traits?.has(e)}),Object.defineProperty(o,"name",{value:e}),o}var je=class extends Error{constructor(){super(`Encountered Promise during synchronous parse. Use .parseAsync() instead.`)}},Me=class extends Error{constructor(e){super(`Encountered unidirectional transform during encode: ${e}`),this.name=`ZodEncodeError`}};(Ae=globalThis).__zod_globalConfig??(Ae.__zod_globalConfig={});var Ne=globalThis.__zod_globalConfig;function Pe(e){return e&&Object.assign(Ne,e),Ne}function Fe(e){let t=Object.values(e).filter(e=>typeof e==`number`);return Object.entries(e).filter(([e,n])=>t.indexOf(+e)===-1).map(([e,t])=>t)}function Ie(e,t){return typeof t==`bigint`?t.toString():t}function Le(e){return{get value(){{let t=e();return Object.defineProperty(this,"value",{value:t}),t}throw Error(`cached value already set`)}}}function Re(e){return e==null}function ze(e){let t=+!!e.startsWith(`^`),n=e.endsWith(`$`)?e.length-1:e.length;return e.slice(t,n)}function Be(e,t){let n=e/t,r=Math.round(n),i=2**-52*Math.max(Math.abs(n),1);return Math.abs(n-r)<i?0:n-r}var Ve=Symbol(`evaluating`);function R(e,t,n){let r;Object.defineProperty(e,t,{get(){if(r!==Ve)return r===void 0&&(r=Ve,r=n()),r},set(n){Object.defineProperty(e,t,{value:n})},configurable:!0})}function He(e,t,n){Object.defineProperty(e,t,{value:n,writable:!0,enumerable:!0,configurable:!0})}function Ue(...e){let t={};for(let n of e){let e=Object.getOwnPropertyDescriptors(n);Object.assign(t,e)}return Object.defineProperties({},t)}function We(e){return JSON.stringify(e)}function Ge(e){return e.toLowerCase().trim().replace(/[^\w\s-]/g,``).replace(/[\s_-]+/g,`-`).replace(/^-+|-+$/g,``)}var Ke=`captureStackTrace`in Error?Error.captureStackTrace:(...e)=>{};function qe(e){return typeof e==`object`&&!!e&&!Array.isArray(e)}var Je=Le(()=>{if(Ne.jitless||typeof navigator<`u`&&navigator?.userAgent?.includes(`Cloudflare`))return!1;try{return Function(``),!0}catch{return!1}});function Ye(e){if(qe(e)===!1)return!1;let t=e.constructor;if(t===void 0||typeof t!=`function`)return!0;let n=t.prototype;return!(qe(n)===!1||Object.prototype.hasOwnProperty.call(n,`isPrototypeOf`)===!1)}function Xe(e){return Ye(e)?{...e}:Array.isArray(e)?[...e]:e instanceof Map?new Map(e):e instanceof Set?new Set(e):e}var Ze=new Set([`string`,`number`,`symbol`]);function Qe(e){return e.replace(/[.*+?^${}()|[\]\\]/g,`\\$&`)}function $e(e,t,n){let r=new e._zod.constr(t??e._zod.def);return(!t||n?.parent)&&(r._zod.parent=e),r}function z(e){let t=e;if(!t)return{};if(typeof t==`string`)return{error:()=>t};if(t?.message!==void 0){if(t?.error!==void 0)throw Error("Cannot specify both `message` and `error` params");t.error=t.message}return delete t.message,typeof t.error==`string`?{...t,error:()=>t.error}:t}function et(e){return Object.keys(e).filter(t=>e[t]._zod.optin===`optional`&&e[t]._zod.optout===`optional`)}var tt={safeint:[-(2**53-1),2**53-1],int32:[-2147483648,2147483647],uint32:[0,4294967295],float32:[-34028234663852886e22,34028234663852886e22],float64:[-Number.MAX_VALUE,Number.MAX_VALUE]};function nt(e,t){let n=e._zod.def,r=n.checks;if(r&&r.length>0)throw Error(`.pick() cannot be used on object schemas containing refinements`);return $e(e,Ue(e._zod.def,{get shape(){let e={};for(let r in t){if(!(r in n.shape))throw Error(`Unrecognized key: "${r}"`);t[r]&&(e[r]=n.shape[r])}return He(this,`shape`,e),e},checks:[]}))}function rt(e,t){let n=e._zod.def,r=n.checks;if(r&&r.length>0)throw Error(`.omit() cannot be used on object schemas containing refinements`);return $e(e,Ue(e._zod.def,{get shape(){let r={...e._zod.def.shape};for(let e in t){if(!(e in n.shape))throw Error(`Unrecognized key: "${e}"`);t[e]&&delete r[e]}return He(this,`shape`,r),r},checks:[]}))}function it(e,t){if(!Ye(t))throw Error(`Invalid input to extend: expected a plain object`);let n=e._zod.def.checks;if(n&&n.length>0){let n=e._zod.def.shape;for(let e in t)if(Object.getOwnPropertyDescriptor(n,e)!==void 0)throw Error("Cannot overwrite keys on object schemas containing refinements. Use `.safeExtend()` instead.")}return $e(e,Ue(e._zod.def,{get shape(){let n={...e._zod.def.shape,...t};return He(this,`shape`,n),n}}))}function at(e,t){if(!Ye(t))throw Error(`Invalid input to safeExtend: expected a plain object`);return $e(e,Ue(e._zod.def,{get shape(){let n={...e._zod.def.shape,...t};return He(this,`shape`,n),n}}))}function ot(e,t){if(e._zod.def.checks?.length)throw Error(`.merge() cannot be used on object schemas containing refinements. Use .safeExtend() instead.`);return $e(e,Ue(e._zod.def,{get shape(){let n={...e._zod.def.shape,...t._zod.def.shape};return He(this,`shape`,n),n},get catchall(){return t._zod.def.catchall},checks:t._zod.def.checks??[]}))}function st(e,t,n){let r=t._zod.def.checks;if(r&&r.length>0)throw Error(`.partial() cannot be used on object schemas containing refinements`);return $e(t,Ue(t._zod.def,{get shape(){let r=t._zod.def.shape,i={...r};if(n)for(let t in n){if(!(t in r))throw Error(`Unrecognized key: "${t}"`);n[t]&&(i[t]=e?new e({type:`optional`,innerType:r[t]}):r[t])}else for(let t in r)i[t]=e?new e({type:`optional`,innerType:r[t]}):r[t];return He(this,`shape`,i),i},checks:[]}))}function ct(e,t,n){return $e(t,Ue(t._zod.def,{get shape(){let r=t._zod.def.shape,i={...r};if(n)for(let t in n){if(!(t in i))throw Error(`Unrecognized key: "${t}"`);n[t]&&(i[t]=new e({type:`nonoptional`,innerType:r[t]}))}else for(let t in r)i[t]=new e({type:`nonoptional`,innerType:r[t]});return He(this,`shape`,i),i}}))}function lt(e,t=0){if(e.aborted===!0)return!0;for(let n=t;n<e.issues.length;n++)if(e.issues[n]?.continue!==!0)return!0;return!1}function ut(e,t=0){if(e.aborted===!0)return!0;for(let n=t;n<e.issues.length;n++)if(e.issues[n]?.continue===!1)return!0;return!1}function dt(e,t){return t.map(t=>{var n;return(n=t).path??(n.path=[]),t.path.unshift(e),t})}function ft(e){return typeof e==`string`?e:e?.message}function pt(e,t,n){let r=e.message?e.message:ft(e.inst?._zod.def?.error?.(e))??ft(t?.error?.(e))??ft(n.customError?.(e))??ft(n.localeError?.(e))??`Invalid input`,{inst:i,continue:a,input:o,...s}=e;return s.path??=[],s.message=r,t?.reportInput&&(s.input=o),s}function mt(e){return Array.isArray(e)?`array`:typeof e==`string`?`string`:`unknown`}function ht(...e){let[t,n,r]=e;return typeof t==`string`?{message:t,code:`custom`,input:n,inst:r}:{...t}}var gt=(e,t)=>{e.name=`$ZodError`,Object.defineProperty(e,"_zod",{value:e._zod,enumerable:!1}),Object.defineProperty(e,"issues",{value:t,enumerable:!1}),e.message=JSON.stringify(t,Ie,2),Object.defineProperty(e,"toString",{value:()=>e.message,enumerable:!1})},_t=L(`$ZodError`,gt),vt=L(`$ZodError`,gt,{Parent:Error});function yt(e,t=e=>e.message){let n={},r=[];for(let i of e.issues)i.path.length>0?(n[i.path[0]]=n[i.path[0]]||[],n[i.path[0]].push(t(i))):r.push(t(i));return{formErrors:r,fieldErrors:n}}function bt(e,t=e=>e.message){let n={_errors:[]},r=(e,i=[])=>{for(let a of e.issues)if(a.code===`invalid_union`&&a.errors.length)a.errors.map(e=>r({issues:e},[...i,...a.path]));else if(a.code===`invalid_key`)r({issues:a.issues},[...i,...a.path]);else if(a.code===`invalid_element`)r({issues:a.issues},[...i,...a.path]);else{let e=[...i,...a.path];if(e.length===0)n._errors.push(t(a));else{let r=n,i=0;for(;i<e.length;){let n=e[i];i===e.length-1?(r[n]=r[n]||{_errors:[]},r[n]._errors.push(t(a))):r[n]=r[n]||{_errors:[]},r=r[n],i++}}}};return r(e),n}var xt=e=>(t,n,r,i)=>{let a=r?{...r,async:!1}:{async:!1},o=t._zod.run({value:n,issues:[]},a);if(o instanceof Promise)throw new je;if(o.issues.length){let t=new((i?.Err)??e)(o.issues.map(e=>pt(e,a,Pe())));throw Ke(t,i?.callee),t}return o.value},St=e=>async(t,n,r,i)=>{let a=r?{...r,async:!0}:{async:!0},o=t._zod.run({value:n,issues:[]},a);if(o instanceof Promise&&(o=await o),o.issues.length){let t=new((i?.Err)??e)(o.issues.map(e=>pt(e,a,Pe())));throw Ke(t,i?.callee),t}return o.value},Ct=e=>(t,n,r)=>{let i=r?{...r,async:!1}:{async:!1},a=t._zod.run({value:n,issues:[]},i);if(a instanceof Promise)throw new je;return a.issues.length?{success:!1,error:new(e??_t)(a.issues.map(e=>pt(e,i,Pe())))}:{success:!0,data:a.value}},wt=Ct(vt),Tt=e=>async(t,n,r)=>{let i=r?{...r,async:!0}:{async:!0},a=t._zod.run({value:n,issues:[]},i);return a instanceof Promise&&(a=await a),a.issues.length?{success:!1,error:new e(a.issues.map(e=>pt(e,i,Pe())))}:{success:!0,data:a.value}},Et=Tt(vt),Dt=e=>(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return xt(e)(t,n,i)},Ot=e=>(t,n,r)=>xt(e)(t,n,r),kt=e=>async(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return St(e)(t,n,i)},At=e=>async(t,n,r)=>St(e)(t,n,r),jt=e=>(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return Ct(e)(t,n,i)},Mt=e=>(t,n,r)=>Ct(e)(t,n,r),Nt=e=>async(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return Tt(e)(t,n,i)},Pt=e=>async(t,n,r)=>Tt(e)(t,n,r),Ft=/^[cC][0-9a-z]{6,}$/,It=/^[0-9a-z]+$/,Lt=/^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$/,Rt=/^[0-9a-vA-V]{20}$/,zt=/^[A-Za-z0-9]{27}$/,Bt=/^[a-zA-Z0-9_-]{21}$/,Vt=/^P(?:(\d+W)|(?!.*W)(?=\d|T\d)(\d+Y)?(\d+M)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+([.,]\d+)?S)?)?)$/,Ht=/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/,Ut=e=>e?RegExp(`^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-${e}[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12})$`):/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}|00000000-0000-0000-0000-000000000000|ffffffff-ffff-ffff-ffff-ffffffffffff)$/,Wt=/^(?!\.)(?!.*\.\.)([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$/,Gt=`^(\\p{Extended_Pictographic}|\\p{Emoji_Component})+$`;function Kt(){return new RegExp(Gt,`u`)}var qt=/^(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])$/,Jt=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:))$/,Yt=/^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\/([0-9]|[1-2][0-9]|3[0-2])$/,Xt=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|::|([0-9a-fA-F]{1,4})?::([0-9a-fA-F]{1,4}:?){0,6})\/(12[0-8]|1[01][0-9]|[1-9]?[0-9])$/,Zt=/^$|^(?:[0-9a-zA-Z+/]{4})*(?:(?:[0-9a-zA-Z+/]{2}==)|(?:[0-9a-zA-Z+/]{3}=))?$/,Qt=/^[A-Za-z0-9_-]*$/,$t=/^https?$/,en=/^\+[1-9]\d{6,14}$/,tn=`(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))`,nn=RegExp(`^${tn}$`);function rn(e){let t=`(?:[01]\\d|2[0-3]):[0-5]\\d`;return typeof e.precision==`number`?e.precision===-1?`${t}`:e.precision===0?`${t}:[0-5]\\d`:`${t}:[0-5]\\d\\.\\d{${e.precision}}`:`${t}(?::[0-5]\\d(?:\\.\\d+)?)?`}function an(e){return RegExp(`^${rn(e)}$`)}function on(e){let t=rn({precision:e.precision}),n=[`Z`];e.local&&n.push(``),e.offset&&n.push(`([+-](?:[01]\\d|2[0-3]):[0-5]\\d)`);let r=`${t}(?:${n.join(`|`)})`;return RegExp(`^${tn}T(?:${r})$`)}var sn=e=>{let t=e?`[\\s\\S]{${e?.minimum??0},${e?.maximum??``}}`:`[\\s\\S]*`;return RegExp(`^${t}$`)},cn=/^-?\d+$/,ln=/^-?\d+(?:\.\d+)?$/,un=/^(?:true|false)$/i,dn=/^[^A-Z]*$/,fn=/^[^a-z]*$/,B=L(`$ZodCheck`,(e,t)=>{var n;e._zod??={},e._zod.def=t,(n=e._zod).onattach??(n.onattach=[])}),pn={number:`number`,bigint:`bigint`,object:`date`},mn=L(`$ZodCheckLessThan`,(e,t)=>{B.init(e,t);let n=pn[typeof t.value];e._zod.onattach.push(e=>{let n=e._zod.bag,r=(t.inclusive?n.maximum:n.exclusiveMaximum)??1/0;t.value<r&&(t.inclusive?n.maximum=t.value:n.exclusiveMaximum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value<=t.value:r.value<t.value)||r.issues.push({origin:n,code:`too_big`,maximum:typeof t.value==`object`?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),hn=L(`$ZodCheckGreaterThan`,(e,t)=>{B.init(e,t);let n=pn[typeof t.value];e._zod.onattach.push(e=>{let n=e._zod.bag,r=(t.inclusive?n.minimum:n.exclusiveMinimum)??-1/0;t.value>r&&(t.inclusive?n.minimum=t.value:n.exclusiveMinimum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value>=t.value:r.value>t.value)||r.issues.push({origin:n,code:`too_small`,minimum:typeof t.value==`object`?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),gn=L(`$ZodCheckMultipleOf`,(e,t)=>{B.init(e,t),e._zod.onattach.push(e=>{var n;(n=e._zod.bag).multipleOf??(n.multipleOf=t.value)}),e._zod.check=n=>{if(typeof n.value!=typeof t.value)throw Error(`Cannot mix number and bigint in multiple_of check.`);(typeof n.value==`bigint`?n.value%t.value===BigInt(0):Be(n.value,t.value)===0)||n.issues.push({origin:typeof n.value,code:`not_multiple_of`,divisor:t.value,input:n.value,inst:e,continue:!t.abort})}}),_n=L(`$ZodCheckNumberFormat`,(e,t)=>{B.init(e,t),t.format=t.format||`float64`;let n=t.format?.includes(`int`),r=n?`int`:`number`,[i,a]=tt[t.format];e._zod.onattach.push(e=>{let r=e._zod.bag;r.format=t.format,r.minimum=i,r.maximum=a,n&&(r.pattern=cn)}),e._zod.check=o=>{let s=o.value;if(n){if(!Number.isInteger(s)){o.issues.push({expected:r,format:t.format,code:`invalid_type`,continue:!1,input:s,inst:e});return}if(!Number.isSafeInteger(s)){s>0?o.issues.push({input:s,code:`too_big`,maximum:2**53-1,note:`Integers must be within the safe integer range.`,inst:e,origin:r,inclusive:!0,continue:!t.abort}):o.issues.push({input:s,code:`too_small`,minimum:-(2**53-1),note:`Integers must be within the safe integer range.`,inst:e,origin:r,inclusive:!0,continue:!t.abort});return}}s<i&&o.issues.push({origin:`number`,input:s,code:`too_small`,minimum:i,inclusive:!0,inst:e,continue:!t.abort}),s>a&&o.issues.push({origin:`number`,input:s,code:`too_big`,maximum:a,inclusive:!0,inst:e,continue:!t.abort})}}),vn=L(`$ZodCheckMaxLength`,(e,t)=>{var n;B.init(e,t),(n=e._zod.def).when??(n.when=e=>{let t=e.value;return!Re(t)&&t.length!==void 0}),e._zod.onattach.push(e=>{let n=e._zod.bag.maximum??1/0;t.maximum<n&&(e._zod.bag.maximum=t.maximum)}),e._zod.check=n=>{let r=n.value;if(r.length<=t.maximum)return;let i=mt(r);n.issues.push({origin:i,code:`too_big`,maximum:t.maximum,inclusive:!0,input:r,inst:e,continue:!t.abort})}}),yn=L(`$ZodCheckMinLength`,(e,t)=>{var n;B.init(e,t),(n=e._zod.def).when??(n.when=e=>{let t=e.value;return!Re(t)&&t.length!==void 0}),e._zod.onattach.push(e=>{let n=e._zod.bag.minimum??-1/0;t.minimum>n&&(e._zod.bag.minimum=t.minimum)}),e._zod.check=n=>{let r=n.value;if(r.length>=t.minimum)return;let i=mt(r);n.issues.push({origin:i,code:`too_small`,minimum:t.minimum,inclusive:!0,input:r,inst:e,continue:!t.abort})}}),bn=L(`$ZodCheckLengthEquals`,(e,t)=>{var n;B.init(e,t),(n=e._zod.def).when??(n.when=e=>{let t=e.value;return!Re(t)&&t.length!==void 0}),e._zod.onattach.push(e=>{let n=e._zod.bag;n.minimum=t.length,n.maximum=t.length,n.length=t.length}),e._zod.check=n=>{let r=n.value,i=r.length;if(i===t.length)return;let a=mt(r),o=i>t.length;n.issues.push({origin:a,...o?{code:`too_big`,maximum:t.length}:{code:`too_small`,minimum:t.length},inclusive:!0,exact:!0,input:n.value,inst:e,continue:!t.abort})}}),xn=L(`$ZodCheckStringFormat`,(e,t)=>{var n,r;B.init(e,t),e._zod.onattach.push(e=>{let n=e._zod.bag;n.format=t.format,t.pattern&&(n.patterns??=new Set,n.patterns.add(t.pattern))}),t.pattern?(n=e._zod).check??(n.check=n=>{t.pattern.lastIndex=0,!t.pattern.test(n.value)&&n.issues.push({origin:`string`,code:`invalid_format`,format:t.format,input:n.value,...t.pattern?{pattern:t.pattern.toString()}:{},inst:e,continue:!t.abort})}):(r=e._zod).check??(r.check=()=>{})}),Sn=L(`$ZodCheckRegex`,(e,t)=>{xn.init(e,t),e._zod.check=n=>{t.pattern.lastIndex=0,!t.pattern.test(n.value)&&n.issues.push({origin:`string`,code:`invalid_format`,format:`regex`,input:n.value,pattern:t.pattern.toString(),inst:e,continue:!t.abort})}}),Cn=L(`$ZodCheckLowerCase`,(e,t)=>{t.pattern??=dn,xn.init(e,t)}),wn=L(`$ZodCheckUpperCase`,(e,t)=>{t.pattern??=fn,xn.init(e,t)}),Tn=L(`$ZodCheckIncludes`,(e,t)=>{B.init(e,t);let n=Qe(t.includes),r=new RegExp(typeof t.position==`number`?`^.{${t.position}}${n}`:n);t.pattern=r,e._zod.onattach.push(e=>{let t=e._zod.bag;t.patterns??=new Set,t.patterns.add(r)}),e._zod.check=n=>{n.value.includes(t.includes,t.position)||n.issues.push({origin:`string`,code:`invalid_format`,format:`includes`,includes:t.includes,input:n.value,inst:e,continue:!t.abort})}}),En=L(`$ZodCheckStartsWith`,(e,t)=>{B.init(e,t);let n=RegExp(`^${Qe(t.prefix)}.*`);t.pattern??=n,e._zod.onattach.push(e=>{let t=e._zod.bag;t.patterns??=new Set,t.patterns.add(n)}),e._zod.check=n=>{n.value.startsWith(t.prefix)||n.issues.push({origin:`string`,code:`invalid_format`,format:`starts_with`,prefix:t.prefix,input:n.value,inst:e,continue:!t.abort})}}),Dn=L(`$ZodCheckEndsWith`,(e,t)=>{B.init(e,t);let n=RegExp(`.*${Qe(t.suffix)}$`);t.pattern??=n,e._zod.onattach.push(e=>{let t=e._zod.bag;t.patterns??=new Set,t.patterns.add(n)}),e._zod.check=n=>{n.value.endsWith(t.suffix)||n.issues.push({origin:`string`,code:`invalid_format`,format:`ends_with`,suffix:t.suffix,input:n.value,inst:e,continue:!t.abort})}}),On=L(`$ZodCheckOverwrite`,(e,t)=>{B.init(e,t),e._zod.check=e=>{e.value=t.tx(e.value)}}),kn=class{constructor(e=[]){this.content=[],this.indent=0,this&&(this.args=e)}indented(e){this.indent+=1,e(this),--this.indent}write(e){if(typeof e==`function`){e(this,{execution:`sync`}),e(this,{execution:`async`});return}let t=e.split(`
`).filter(e=>e),n=Math.min(...t.map(e=>e.length-e.trimStart().length)),r=t.map(e=>e.slice(n)).map(e=>` `.repeat(this.indent*2)+e);for(let e of r)this.content.push(e)}compile(){let e=Function,t=this?.args,n=[...(this?.content??[``]).map(e=>`  ${e}`)];return new e(...t,n.join(`
`))}},An={major:4,minor:4,patch:3},V=L(`$ZodType`,(e,t)=>{var n;e??={},e._zod.def=t,e._zod.bag=e._zod.bag||{},e._zod.version=An;let r=[...e._zod.def.checks??[]];e._zod.traits.has(`$ZodCheck`)&&r.unshift(e);for(let t of r)for(let n of t._zod.onattach)n(e);if(r.length===0)(n=e._zod).deferred??(n.deferred=[]),e._zod.deferred?.push(()=>{e._zod.run=e._zod.parse});else{let t=(e,t,n)=>{let r=lt(e),i;for(let a of t){if(a._zod.def.when){if(ut(e)||!a._zod.def.when(e))continue}else if(r)continue;let t=e.issues.length,o=a._zod.check(e);if(o instanceof Promise&&n?.async===!1)throw new je;if(i||o instanceof Promise)i=(i??Promise.resolve()).then(async()=>{await o,e.issues.length!==t&&(r||=lt(e,t))});else{if(e.issues.length===t)continue;r||=lt(e,t)}}return i?i.then(()=>e):e},n=(n,i,a)=>{if(lt(n))return n.aborted=!0,n;let o=t(i,r,a);if(o instanceof Promise){if(a.async===!1)throw new je;return o.then(t=>e._zod.parse(t,a))}return e._zod.parse(o,a)};e._zod.run=(i,a)=>{if(a.skipChecks)return e._zod.parse(i,a);if(a.direction===`backward`){let t=e._zod.parse({value:i.value,issues:[]},{...a,skipChecks:!0});return t instanceof Promise?t.then(e=>n(e,i,a)):n(t,i,a)}let o=e._zod.parse(i,a);if(o instanceof Promise){if(a.async===!1)throw new je;return o.then(e=>t(e,r,a))}return t(o,r,a)}}R(e,`~standard`,()=>({validate:t=>{try{let n=wt(e,t);return n.success?{value:n.data}:{issues:n.error?.issues}}catch{return Et(e,t).then(e=>e.success?{value:e.data}:{issues:e.error?.issues})}},vendor:`zod`,version:1}))}),jn=L(`$ZodString`,(e,t)=>{V.init(e,t),e._zod.pattern=[...e?._zod.bag?.patterns??[]].pop()??sn(e._zod.bag),e._zod.parse=(n,r)=>{if(t.coerce)try{n.value=String(n.value)}catch{}return typeof n.value==`string`||n.issues.push({expected:`string`,code:`invalid_type`,input:n.value,inst:e}),n}}),H=L(`$ZodStringFormat`,(e,t)=>{xn.init(e,t),jn.init(e,t)}),Mn=L(`$ZodGUID`,(e,t)=>{t.pattern??=Ht,H.init(e,t)}),Nn=L(`$ZodUUID`,(e,t)=>{if(t.version){let e={v1:1,v2:2,v3:3,v4:4,v5:5,v6:6,v7:7,v8:8}[t.version];if(e===void 0)throw Error(`Invalid UUID version: "${t.version}"`);t.pattern??=Ut(e)}else t.pattern??=Ut();H.init(e,t)}),Pn=L(`$ZodEmail`,(e,t)=>{t.pattern??=Wt,H.init(e,t)}),Fn=L(`$ZodURL`,(e,t)=>{H.init(e,t),e._zod.check=n=>{try{let r=n.value.trim();if(!t.normalize&&t.protocol?.source===$t.source&&!/^https?:\/\//i.test(r)){n.issues.push({code:`invalid_format`,format:`url`,note:`Invalid URL format`,input:n.value,inst:e,continue:!t.abort});return}let i=new URL(r);t.hostname&&(t.hostname.lastIndex=0,t.hostname.test(i.hostname)||n.issues.push({code:`invalid_format`,format:`url`,note:`Invalid hostname`,pattern:t.hostname.source,input:n.value,inst:e,continue:!t.abort})),t.protocol&&(t.protocol.lastIndex=0,t.protocol.test(i.protocol.endsWith(`:`)?i.protocol.slice(0,-1):i.protocol)||n.issues.push({code:`invalid_format`,format:`url`,note:`Invalid protocol`,pattern:t.protocol.source,input:n.value,inst:e,continue:!t.abort})),t.normalize?n.value=i.href:n.value=r;return}catch{n.issues.push({code:`invalid_format`,format:`url`,input:n.value,inst:e,continue:!t.abort})}}}),In=L(`$ZodEmoji`,(e,t)=>{t.pattern??=Kt(),H.init(e,t)}),Ln=L(`$ZodNanoID`,(e,t)=>{t.pattern??=Bt,H.init(e,t)}),Rn=L(`$ZodCUID`,(e,t)=>{t.pattern??=Ft,H.init(e,t)}),zn=L(`$ZodCUID2`,(e,t)=>{t.pattern??=It,H.init(e,t)}),Bn=L(`$ZodULID`,(e,t)=>{t.pattern??=Lt,H.init(e,t)}),Vn=L(`$ZodXID`,(e,t)=>{t.pattern??=Rt,H.init(e,t)}),Hn=L(`$ZodKSUID`,(e,t)=>{t.pattern??=zt,H.init(e,t)}),Un=L(`$ZodISODateTime`,(e,t)=>{t.pattern??=on(t),H.init(e,t)}),Wn=L(`$ZodISODate`,(e,t)=>{t.pattern??=nn,H.init(e,t)}),Gn=L(`$ZodISOTime`,(e,t)=>{t.pattern??=an(t),H.init(e,t)}),Kn=L(`$ZodISODuration`,(e,t)=>{t.pattern??=Vt,H.init(e,t)}),qn=L(`$ZodIPv4`,(e,t)=>{t.pattern??=qt,H.init(e,t),e._zod.bag.format=`ipv4`}),Jn=L(`$ZodIPv6`,(e,t)=>{t.pattern??=Jt,H.init(e,t),e._zod.bag.format=`ipv6`,e._zod.check=n=>{try{new URL(`http://[${n.value}]`)}catch{n.issues.push({code:`invalid_format`,format:`ipv6`,input:n.value,inst:e,continue:!t.abort})}}}),Yn=L(`$ZodCIDRv4`,(e,t)=>{t.pattern??=Yt,H.init(e,t)}),Xn=L(`$ZodCIDRv6`,(e,t)=>{t.pattern??=Xt,H.init(e,t),e._zod.check=n=>{let r=n.value.split(`/`);try{if(r.length!==2)throw Error();let[e,t]=r;if(!t)throw Error();let n=Number(t);if(`${n}`!==t||n<0||n>128)throw Error();new URL(`http://[${e}]`)}catch{n.issues.push({code:`invalid_format`,format:`cidrv6`,input:n.value,inst:e,continue:!t.abort})}}});function Zn(e){if(e===``)return!0;if(/\s/.test(e)||e.length%4!=0)return!1;try{return atob(e),!0}catch{return!1}}var Qn=L(`$ZodBase64`,(e,t)=>{t.pattern??=Zt,H.init(e,t),e._zod.bag.contentEncoding=`base64`,e._zod.check=n=>{Zn(n.value)||n.issues.push({code:`invalid_format`,format:`base64`,input:n.value,inst:e,continue:!t.abort})}});function $n(e){if(!Qt.test(e))return!1;let t=e.replace(/[-_]/g,e=>e===`-`?`+`:`/`);return Zn(t.padEnd(Math.ceil(t.length/4)*4,`=`))}var er=L(`$ZodBase64URL`,(e,t)=>{t.pattern??=Qt,H.init(e,t),e._zod.bag.contentEncoding=`base64url`,e._zod.check=n=>{$n(n.value)||n.issues.push({code:`invalid_format`,format:`base64url`,input:n.value,inst:e,continue:!t.abort})}}),tr=L(`$ZodE164`,(e,t)=>{t.pattern??=en,H.init(e,t)});function nr(e,t=null){try{let n=e.split(`.`);if(n.length!==3)return!1;let[r]=n;if(!r)return!1;let i=JSON.parse(atob(r));return!(`typ`in i&&i?.typ!==`JWT`||!i.alg||t&&(!(`alg`in i)||i.alg!==t))}catch{return!1}}var rr=L(`$ZodJWT`,(e,t)=>{H.init(e,t),e._zod.check=n=>{nr(n.value,t.alg)||n.issues.push({code:`invalid_format`,format:`jwt`,input:n.value,inst:e,continue:!t.abort})}}),ir=L(`$ZodNumber`,(e,t)=>{V.init(e,t),e._zod.pattern=e._zod.bag.pattern??ln,e._zod.parse=(n,r)=>{if(t.coerce)try{n.value=Number(n.value)}catch{}let i=n.value;if(typeof i==`number`&&!Number.isNaN(i)&&Number.isFinite(i))return n;let a=typeof i==`number`?Number.isNaN(i)?`NaN`:Number.isFinite(i)?void 0:`Infinity`:void 0;return n.issues.push({expected:`number`,code:`invalid_type`,input:i,inst:e,...a?{received:a}:{}}),n}}),ar=L(`$ZodNumberFormat`,(e,t)=>{_n.init(e,t),ir.init(e,t)}),or=L(`$ZodBoolean`,(e,t)=>{V.init(e,t),e._zod.pattern=un,e._zod.parse=(n,r)=>{if(t.coerce)try{n.value=!!n.value}catch{}let i=n.value;return typeof i==`boolean`||n.issues.push({expected:`boolean`,code:`invalid_type`,input:i,inst:e}),n}}),sr=L(`$ZodUnknown`,(e,t)=>{V.init(e,t),e._zod.parse=e=>e}),cr=L(`$ZodNever`,(e,t)=>{V.init(e,t),e._zod.parse=(t,n)=>(t.issues.push({expected:`never`,code:`invalid_type`,input:t.value,inst:e}),t)});function lr(e,t,n){e.issues.length&&t.issues.push(...dt(n,e.issues)),t.value[n]=e.value}var ur=L(`$ZodArray`,(e,t)=>{V.init(e,t),e._zod.parse=(n,r)=>{let i=n.value;if(!Array.isArray(i))return n.issues.push({expected:`array`,code:`invalid_type`,input:i,inst:e}),n;n.value=Array(i.length);let a=[];for(let e=0;e<i.length;e++){let o=i[e],s=t.element._zod.run({value:o,issues:[]},r);s instanceof Promise?a.push(s.then(t=>lr(t,n,e))):lr(s,n,e)}return a.length?Promise.all(a).then(()=>n):n}});function dr(e,t,n,r,i,a){let o=n in r;if(e.issues.length){if(i&&a&&!o)return;t.issues.push(...dt(n,e.issues))}if(!o&&!i){e.issues.length||t.issues.push({code:`invalid_type`,expected:`nonoptional`,input:void 0,path:[n]});return}e.value===void 0?o&&(t.value[n]=void 0):t.value[n]=e.value}function fr(e){let t=Object.keys(e.shape);for(let n of t)if(!e.shape?.[n]?._zod?.traits?.has(`$ZodType`))throw Error(`Invalid element at key "${n}": expected a Zod schema`);let n=et(e.shape);return{...e,keys:t,keySet:new Set(t),numKeys:t.length,optionalKeys:new Set(n)}}function pr(e,t,n,r,i,a){let o=[],s=i.keySet,c=i.catchall._zod,l=c.def.type,u=c.optin===`optional`,d=c.optout===`optional`;for(let i in t){if(i===`__proto__`||s.has(i))continue;if(l===`never`){o.push(i);continue}let a=c.run({value:t[i],issues:[]},r);a instanceof Promise?e.push(a.then(e=>dr(e,n,i,t,u,d))):dr(a,n,i,t,u,d)}return o.length&&n.issues.push({code:`unrecognized_keys`,keys:o,input:t,inst:a}),e.length?Promise.all(e).then(()=>n):n}var mr=L(`$ZodObject`,(e,t)=>{if(V.init(e,t),!Object.getOwnPropertyDescriptor(t,`shape`)?.get){let e=t.shape;Object.defineProperty(t,"shape",{get:()=>{let n={...e};return Object.defineProperty(t,"shape",{value:n}),n}})}let n=Le(()=>fr(t));R(e._zod,`propValues`,()=>{let e=t.shape,n={};for(let t in e){let r=e[t]._zod;if(r.values){n[t]??(n[t]=new Set);for(let e of r.values)n[t].add(e)}}return n});let r=qe,i=t.catchall,a;e._zod.parse=(t,o)=>{a??=n.value;let s=t.value;if(!r(s))return t.issues.push({expected:`object`,code:`invalid_type`,input:s,inst:e}),t;t.value={};let c=[],l=a.shape;for(let e of a.keys){let n=l[e],r=n._zod.optin===`optional`,i=n._zod.optout===`optional`,a=n._zod.run({value:s[e],issues:[]},o);a instanceof Promise?c.push(a.then(n=>dr(n,t,e,s,r,i))):dr(a,t,e,s,r,i)}return i?pr(c,s,t,o,n.value,e):c.length?Promise.all(c).then(()=>t):t}}),hr=L(`$ZodObjectJIT`,(e,t)=>{mr.init(e,t);let n=e._zod.parse,r=Le(()=>fr(t)),i=e=>{let t=new kn([`shape`,`payload`,`ctx`]),n=r.value,i=e=>{let t=We(e);return`shape[${t}]._zod.run({ value: input[${t}], issues: [] }, ctx)`};t.write(`const input = payload.value;`);let a=Object.create(null),o=0;for(let e of n.keys)a[e]=`key_${o++}`;t.write(`const newResult = {};`);for(let r of n.keys){let n=a[r],o=We(r),s=e[r],c=s?._zod?.optin===`optional`,l=s?._zod?.optout===`optional`;t.write(`const ${n} = ${i(r)};`),c&&l?t.write(`
        if (${n}.issues.length) {
          if (${o} in input) {
            payload.issues = payload.issues.concat(${n}.issues.map(iss => ({
              ...iss,
              path: iss.path ? [${o}, ...iss.path] : [${o}]
            })));
          }
        }
        
        if (${n}.value === undefined) {
          if (${o} in input) {
            newResult[${o}] = undefined;
          }
        } else {
          newResult[${o}] = ${n}.value;
        }
        
      `):c?t.write(`
        if (${n}.issues.length) {
          payload.issues = payload.issues.concat(${n}.issues.map(iss => ({
            ...iss,
            path: iss.path ? [${o}, ...iss.path] : [${o}]
          })));
        }
        
        if (${n}.value === undefined) {
          if (${o} in input) {
            newResult[${o}] = undefined;
          }
        } else {
          newResult[${o}] = ${n}.value;
        }
        
      `):t.write(`
        const ${n}_present = ${o} in input;
        if (${n}.issues.length) {
          payload.issues = payload.issues.concat(${n}.issues.map(iss => ({
            ...iss,
            path: iss.path ? [${o}, ...iss.path] : [${o}]
          })));
        }
        if (!${n}_present && !${n}.issues.length) {
          payload.issues.push({
            code: "invalid_type",
            expected: "nonoptional",
            input: undefined,
            path: [${o}]
          });
        }

        if (${n}_present) {
          if (${n}.value === undefined) {
            newResult[${o}] = undefined;
          } else {
            newResult[${o}] = ${n}.value;
          }
        }

      `)}t.write(`payload.value = newResult;`),t.write(`return payload;`);let s=t.compile();return(t,n)=>s(e,t,n)},a,o=qe,s=!Ne.jitless,c=s&&Je.value,l=t.catchall,u;e._zod.parse=(d,f)=>{u??=r.value;let p=d.value;return o(p)?s&&c&&f?.async===!1&&f.jitless!==!0?(a||=i(t.shape),d=a(d,f),l?pr([],p,d,f,u,e):d):n(d,f):(d.issues.push({expected:`object`,code:`invalid_type`,input:p,inst:e}),d)}});function gr(e,t,n,r){for(let n of e)if(n.issues.length===0)return t.value=n.value,t;let i=e.filter(e=>!lt(e));return i.length===1?(t.value=i[0].value,i[0]):(t.issues.push({code:`invalid_union`,input:t.value,inst:n,errors:e.map(e=>e.issues.map(e=>pt(e,r,Pe())))}),t)}var _r=L(`$ZodUnion`,(e,t)=>{V.init(e,t),R(e._zod,`optin`,()=>t.options.some(e=>e._zod.optin===`optional`)?`optional`:void 0),R(e._zod,`optout`,()=>t.options.some(e=>e._zod.optout===`optional`)?`optional`:void 0),R(e._zod,`values`,()=>{if(t.options.every(e=>e._zod.values))return new Set(t.options.flatMap(e=>Array.from(e._zod.values)))}),R(e._zod,`pattern`,()=>{if(t.options.every(e=>e._zod.pattern)){let e=t.options.map(e=>e._zod.pattern);return RegExp(`^(${e.map(e=>ze(e.source)).join(`|`)})$`)}});let n=t.options.length===1?t.options[0]._zod.run:null;e._zod.parse=(r,i)=>{if(n)return n(r,i);let a=!1,o=[];for(let e of t.options){let t=e._zod.run({value:r.value,issues:[]},i);if(t instanceof Promise)o.push(t),a=!0;else{if(t.issues.length===0)return t;o.push(t)}}return a?Promise.all(o).then(t=>gr(t,r,e,i)):gr(o,r,e,i)}}),vr=L(`$ZodIntersection`,(e,t)=>{V.init(e,t),e._zod.parse=(e,n)=>{let r=e.value,i=t.left._zod.run({value:r,issues:[]},n),a=t.right._zod.run({value:r,issues:[]},n);return i instanceof Promise||a instanceof Promise?Promise.all([i,a]).then(([t,n])=>br(e,t,n)):br(e,i,a)}});function yr(e,t){if(e===t||e instanceof Date&&t instanceof Date&&+e==+t)return{valid:!0,data:e};if(Ye(e)&&Ye(t)){let n=Object.keys(t),r=Object.keys(e).filter(e=>n.indexOf(e)!==-1),i={...e,...t};for(let n of r){let r=yr(e[n],t[n]);if(!r.valid)return{valid:!1,mergeErrorPath:[n,...r.mergeErrorPath]};i[n]=r.data}return{valid:!0,data:i}}if(Array.isArray(e)&&Array.isArray(t)){if(e.length!==t.length)return{valid:!1,mergeErrorPath:[]};let n=[];for(let r=0;r<e.length;r++){let i=e[r],a=t[r],o=yr(i,a);if(!o.valid)return{valid:!1,mergeErrorPath:[r,...o.mergeErrorPath]};n.push(o.data)}return{valid:!0,data:n}}return{valid:!1,mergeErrorPath:[]}}function br(e,t,n){let r=new Map,i;for(let n of t.issues)if(n.code===`unrecognized_keys`){i??=n;for(let e of n.keys)r.has(e)||r.set(e,{}),r.get(e).l=!0}else e.issues.push(n);for(let t of n.issues)if(t.code===`unrecognized_keys`)for(let e of t.keys)r.has(e)||r.set(e,{}),r.get(e).r=!0;else e.issues.push(t);let a=[...r].filter(([,e])=>e.l&&e.r).map(([e])=>e);if(a.length&&i&&e.issues.push({...i,keys:a}),lt(e))return e;let o=yr(t.value,n.value);if(!o.valid)throw Error(`Unmergable intersection. Error path: ${JSON.stringify(o.mergeErrorPath)}`);return e.value=o.data,e}var xr=L(`$ZodRecord`,(e,t)=>{V.init(e,t),e._zod.parse=(n,r)=>{let i=n.value;if(!Ye(i))return n.issues.push({expected:`record`,code:`invalid_type`,input:i,inst:e}),n;let a=[],o=t.keyType._zod.values;if(o){n.value={};let s=new Set;for(let c of o)if(typeof c==`string`||typeof c==`number`||typeof c==`symbol`){s.add(typeof c==`number`?c.toString():c);let o=t.keyType._zod.run({value:c,issues:[]},r);if(o instanceof Promise)throw Error(`Async schemas not supported in object keys currently`);if(o.issues.length){n.issues.push({code:`invalid_key`,origin:`record`,issues:o.issues.map(e=>pt(e,r,Pe())),input:c,path:[c],inst:e});continue}let l=o.value,u=t.valueType._zod.run({value:i[c],issues:[]},r);u instanceof Promise?a.push(u.then(e=>{e.issues.length&&n.issues.push(...dt(c,e.issues)),n.value[l]=e.value})):(u.issues.length&&n.issues.push(...dt(c,u.issues)),n.value[l]=u.value)}let c;for(let e in i)s.has(e)||(c??=[],c.push(e));c&&c.length>0&&n.issues.push({code:`unrecognized_keys`,input:i,inst:e,keys:c})}else{n.value={};for(let o of Reflect.ownKeys(i)){if(o===`__proto__`||!Object.prototype.propertyIsEnumerable.call(i,o))continue;let s=t.keyType._zod.run({value:o,issues:[]},r);if(s instanceof Promise)throw Error(`Async schemas not supported in object keys currently`);if(typeof o==`string`&&ln.test(o)&&s.issues.length){let e=t.keyType._zod.run({value:Number(o),issues:[]},r);if(e instanceof Promise)throw Error(`Async schemas not supported in object keys currently`);e.issues.length===0&&(s=e)}if(s.issues.length){t.mode===`loose`?n.value[o]=i[o]:n.issues.push({code:`invalid_key`,origin:`record`,issues:s.issues.map(e=>pt(e,r,Pe())),input:o,path:[o],inst:e});continue}let c=t.valueType._zod.run({value:i[o],issues:[]},r);c instanceof Promise?a.push(c.then(e=>{e.issues.length&&n.issues.push(...dt(o,e.issues)),n.value[s.value]=e.value})):(c.issues.length&&n.issues.push(...dt(o,c.issues)),n.value[s.value]=c.value)}}return a.length?Promise.all(a).then(()=>n):n}}),Sr=L(`$ZodEnum`,(e,t)=>{V.init(e,t);let n=Fe(t.entries),r=new Set(n);e._zod.values=r,e._zod.pattern=RegExp(`^(${n.filter(e=>Ze.has(typeof e)).map(e=>typeof e==`string`?Qe(e):e.toString()).join(`|`)})$`),e._zod.parse=(t,i)=>{let a=t.value;return r.has(a)||t.issues.push({code:`invalid_value`,values:n,input:a,inst:e}),t}}),Cr=L(`$ZodTransform`,(e,t)=>{V.init(e,t),e._zod.optin=`optional`,e._zod.parse=(n,r)=>{if(r.direction===`backward`)throw new Me(e.constructor.name);let i=t.transform(n.value,n);if(r.async)return(i instanceof Promise?i:Promise.resolve(i)).then(e=>(n.value=e,n.fallback=!0,n));if(i instanceof Promise)throw new je;return n.value=i,n.fallback=!0,n}});function wr(e,t){return t===void 0&&(e.issues.length||e.fallback)?{issues:[],value:void 0}:e}var Tr=L(`$ZodOptional`,(e,t)=>{V.init(e,t),e._zod.optin=`optional`,e._zod.optout=`optional`,R(e._zod,`values`,()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,void 0]):void 0),R(e._zod,`pattern`,()=>{let e=t.innerType._zod.pattern;return e?RegExp(`^(${ze(e.source)})?$`):void 0}),e._zod.parse=(e,n)=>{if(t.innerType._zod.optin===`optional`){let r=e.value,i=t.innerType._zod.run(e,n);return i instanceof Promise?i.then(e=>wr(e,r)):wr(i,r)}return e.value===void 0?e:t.innerType._zod.run(e,n)}}),Er=L(`$ZodExactOptional`,(e,t)=>{Tr.init(e,t),R(e._zod,`values`,()=>t.innerType._zod.values),R(e._zod,`pattern`,()=>t.innerType._zod.pattern),e._zod.parse=(e,n)=>t.innerType._zod.run(e,n)}),Dr=L(`$ZodNullable`,(e,t)=>{V.init(e,t),R(e._zod,`optin`,()=>t.innerType._zod.optin),R(e._zod,`optout`,()=>t.innerType._zod.optout),R(e._zod,`pattern`,()=>{let e=t.innerType._zod.pattern;return e?RegExp(`^(${ze(e.source)}|null)$`):void 0}),R(e._zod,`values`,()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,null]):void 0),e._zod.parse=(e,n)=>e.value===null?e:t.innerType._zod.run(e,n)}),Or=L(`$ZodDefault`,(e,t)=>{V.init(e,t),e._zod.optin=`optional`,R(e._zod,`values`,()=>t.innerType._zod.values),e._zod.parse=(e,n)=>{if(n.direction===`backward`)return t.innerType._zod.run(e,n);if(e.value===void 0)return e.value=t.defaultValue,e;let r=t.innerType._zod.run(e,n);return r instanceof Promise?r.then(e=>kr(e,t)):kr(r,t)}});function kr(e,t){return e.value===void 0&&(e.value=t.defaultValue),e}var Ar=L(`$ZodPrefault`,(e,t)=>{V.init(e,t),e._zod.optin=`optional`,R(e._zod,`values`,()=>t.innerType._zod.values),e._zod.parse=(e,n)=>(n.direction===`backward`||e.value===void 0&&(e.value=t.defaultValue),t.innerType._zod.run(e,n))}),jr=L(`$ZodNonOptional`,(e,t)=>{V.init(e,t),R(e._zod,`values`,()=>{let e=t.innerType._zod.values;return e?new Set([...e].filter(e=>e!==void 0)):void 0}),e._zod.parse=(n,r)=>{let i=t.innerType._zod.run(n,r);return i instanceof Promise?i.then(t=>Mr(t,e)):Mr(i,e)}});function Mr(e,t){return!e.issues.length&&e.value===void 0&&e.issues.push({code:`invalid_type`,expected:`nonoptional`,input:e.value,inst:t}),e}var Nr=L(`$ZodCatch`,(e,t)=>{V.init(e,t),e._zod.optin=`optional`,R(e._zod,`optout`,()=>t.innerType._zod.optout),R(e._zod,`values`,()=>t.innerType._zod.values),e._zod.parse=(e,n)=>{if(n.direction===`backward`)return t.innerType._zod.run(e,n);let r=t.innerType._zod.run(e,n);return r instanceof Promise?r.then(r=>(e.value=r.value,r.issues.length&&(e.value=t.catchValue({...e,error:{issues:r.issues.map(e=>pt(e,n,Pe()))},input:e.value}),e.issues=[],e.fallback=!0),e)):(e.value=r.value,r.issues.length&&(e.value=t.catchValue({...e,error:{issues:r.issues.map(e=>pt(e,n,Pe()))},input:e.value}),e.issues=[],e.fallback=!0),e)}}),Pr=L(`$ZodPipe`,(e,t)=>{V.init(e,t),R(e._zod,`values`,()=>t.in._zod.values),R(e._zod,`optin`,()=>t.in._zod.optin),R(e._zod,`optout`,()=>t.out._zod.optout),R(e._zod,`propValues`,()=>t.in._zod.propValues),e._zod.parse=(e,n)=>{if(n.direction===`backward`){let r=t.out._zod.run(e,n);return r instanceof Promise?r.then(e=>Fr(e,t.in,n)):Fr(r,t.in,n)}let r=t.in._zod.run(e,n);return r instanceof Promise?r.then(e=>Fr(e,t.out,n)):Fr(r,t.out,n)}});function Fr(e,t,n){return e.issues.length?(e.aborted=!0,e):t._zod.run({value:e.value,issues:e.issues,fallback:e.fallback},n)}var Ir=L(`$ZodReadonly`,(e,t)=>{V.init(e,t),R(e._zod,`propValues`,()=>t.innerType._zod.propValues),R(e._zod,`values`,()=>t.innerType._zod.values),R(e._zod,`optin`,()=>t.innerType?._zod?.optin),R(e._zod,`optout`,()=>t.innerType?._zod?.optout),e._zod.parse=(e,n)=>{if(n.direction===`backward`)return t.innerType._zod.run(e,n);let r=t.innerType._zod.run(e,n);return r instanceof Promise?r.then(Lr):Lr(r)}});function Lr(e){return e.value=Object.freeze(e.value),e}var Rr=L(`$ZodCustom`,(e,t)=>{B.init(e,t),V.init(e,t),e._zod.parse=(e,t)=>e,e._zod.check=n=>{let r=n.value,i=t.fn(r);if(i instanceof Promise)return i.then(t=>zr(t,n,r,e));zr(i,n,r,e)}});function zr(e,t,n,r){if(!e){let e={code:`custom`,input:n,inst:r,path:[...r._zod.def.path??[]],continue:!r._zod.def.abort};r._zod.def.params&&(e.params=r._zod.def.params),t.issues.push(ht(e))}}var Br,Vr=class{constructor(){this._map=new WeakMap,this._idmap=new Map}add(e,...t){let n=t[0];return this._map.set(e,n),n&&typeof n==`object`&&`id`in n&&this._idmap.set(n.id,e),this}clear(){return this._map=new WeakMap,this._idmap=new Map,this}remove(e){let t=this._map.get(e);return t&&typeof t==`object`&&`id`in t&&this._idmap.delete(t.id),this._map.delete(e),this}get(e){let t=e._zod.parent;if(t){let n={...this.get(t)??{}};delete n.id;let r={...n,...this._map.get(e)};return Object.keys(r).length?r:void 0}return this._map.get(e)}has(e){return this._map.has(e)}};function Hr(){return new Vr}(Br=globalThis).__zod_globalRegistry??(Br.__zod_globalRegistry=Hr());var Ur=globalThis.__zod_globalRegistry;function Wr(e,t){return new e({type:`string`,...z(t)})}function Gr(e,t){return new e({type:`string`,format:`email`,check:`string_format`,abort:!1,...z(t)})}function Kr(e,t){return new e({type:`string`,format:`guid`,check:`string_format`,abort:!1,...z(t)})}function qr(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,...z(t)})}function Jr(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,version:`v4`,...z(t)})}function Yr(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,version:`v6`,...z(t)})}function Xr(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,version:`v7`,...z(t)})}function Zr(e,t){return new e({type:`string`,format:`url`,check:`string_format`,abort:!1,...z(t)})}function Qr(e,t){return new e({type:`string`,format:`emoji`,check:`string_format`,abort:!1,...z(t)})}function $r(e,t){return new e({type:`string`,format:`nanoid`,check:`string_format`,abort:!1,...z(t)})}function ei(e,t){return new e({type:`string`,format:`cuid`,check:`string_format`,abort:!1,...z(t)})}function ti(e,t){return new e({type:`string`,format:`cuid2`,check:`string_format`,abort:!1,...z(t)})}function ni(e,t){return new e({type:`string`,format:`ulid`,check:`string_format`,abort:!1,...z(t)})}function ri(e,t){return new e({type:`string`,format:`xid`,check:`string_format`,abort:!1,...z(t)})}function ii(e,t){return new e({type:`string`,format:`ksuid`,check:`string_format`,abort:!1,...z(t)})}function ai(e,t){return new e({type:`string`,format:`ipv4`,check:`string_format`,abort:!1,...z(t)})}function oi(e,t){return new e({type:`string`,format:`ipv6`,check:`string_format`,abort:!1,...z(t)})}function si(e,t){return new e({type:`string`,format:`cidrv4`,check:`string_format`,abort:!1,...z(t)})}function ci(e,t){return new e({type:`string`,format:`cidrv6`,check:`string_format`,abort:!1,...z(t)})}function li(e,t){return new e({type:`string`,format:`base64`,check:`string_format`,abort:!1,...z(t)})}function ui(e,t){return new e({type:`string`,format:`base64url`,check:`string_format`,abort:!1,...z(t)})}function di(e,t){return new e({type:`string`,format:`e164`,check:`string_format`,abort:!1,...z(t)})}function fi(e,t){return new e({type:`string`,format:`jwt`,check:`string_format`,abort:!1,...z(t)})}function pi(e,t){return new e({type:`string`,format:`datetime`,check:`string_format`,offset:!1,local:!1,precision:null,...z(t)})}function mi(e,t){return new e({type:`string`,format:`date`,check:`string_format`,...z(t)})}function hi(e,t){return new e({type:`string`,format:`time`,check:`string_format`,precision:null,...z(t)})}function gi(e,t){return new e({type:`string`,format:`duration`,check:`string_format`,...z(t)})}function _i(e,t){return new e({type:`number`,checks:[],...z(t)})}function vi(e,t){return new e({type:`number`,check:`number_format`,abort:!1,format:`safeint`,...z(t)})}function yi(e,t){return new e({type:`boolean`,...z(t)})}function bi(e){return new e({type:`unknown`})}function xi(e,t){return new e({type:`never`,...z(t)})}function Si(e,t){return new mn({check:`less_than`,...z(t),value:e,inclusive:!1})}function Ci(e,t){return new mn({check:`less_than`,...z(t),value:e,inclusive:!0})}function wi(e,t){return new hn({check:`greater_than`,...z(t),value:e,inclusive:!1})}function Ti(e,t){return new hn({check:`greater_than`,...z(t),value:e,inclusive:!0})}function Ei(e,t){return new gn({check:`multiple_of`,...z(t),value:e})}function Di(e,t){return new vn({check:`max_length`,...z(t),maximum:e})}function Oi(e,t){return new yn({check:`min_length`,...z(t),minimum:e})}function ki(e,t){return new bn({check:`length_equals`,...z(t),length:e})}function Ai(e,t){return new Sn({check:`string_format`,format:`regex`,...z(t),pattern:e})}function ji(e){return new Cn({check:`string_format`,format:`lowercase`,...z(e)})}function Mi(e){return new wn({check:`string_format`,format:`uppercase`,...z(e)})}function Ni(e,t){return new Tn({check:`string_format`,format:`includes`,...z(t),includes:e})}function Pi(e,t){return new En({check:`string_format`,format:`starts_with`,...z(t),prefix:e})}function Fi(e,t){return new Dn({check:`string_format`,format:`ends_with`,...z(t),suffix:e})}function Ii(e){return new On({check:`overwrite`,tx:e})}function Li(e){return Ii(t=>t.normalize(e))}function Ri(){return Ii(e=>e.trim())}function zi(){return Ii(e=>e.toLowerCase())}function Bi(){return Ii(e=>e.toUpperCase())}function Vi(){return Ii(e=>Ge(e))}function Hi(e,t,n){return new e({type:`array`,element:t,...z(n)})}function Ui(e,t,n){return new e({type:`custom`,check:`custom`,fn:t,...z(n)})}function Wi(e,t){let n=Gi(t=>(t.addIssue=e=>{if(typeof e==`string`)t.issues.push(ht(e,t.value,n._zod.def));else{let r=e;r.fatal&&(r.continue=!1),r.code??=`custom`,r.input??=t.value,r.inst??=n,r.continue??=!n._zod.def.abort,t.issues.push(ht(r))}},e(t.value,t)),t);return n}function Gi(e,t){let n=new B({check:`custom`,...z(t)});return n._zod.check=e,n}function Ki(e){let t=e?.target??`draft-2020-12`;return t===`draft-4`&&(t=`draft-04`),t===`draft-7`&&(t=`draft-07`),{processors:e.processors??{},metadataRegistry:e?.metadata??Ur,target:t,unrepresentable:e?.unrepresentable??`throw`,override:e?.override??(()=>{}),io:e?.io??`output`,counter:0,seen:new Map,cycles:e?.cycles??`ref`,reused:e?.reused??`inline`,external:e?.external??void 0}}function U(e,t,n={path:[],schemaPath:[]}){var r;let i=e._zod.def,a=t.seen.get(e);if(a)return a.count++,n.schemaPath.includes(e)&&(a.cycle=n.path),a.schema;let o={schema:{},count:1,cycle:void 0,path:n.path};t.seen.set(e,o);let s=e._zod.toJSONSchema?.();if(s)o.schema=s;else{let r={...n,schemaPath:[...n.schemaPath,e],path:n.path};if(e._zod.processJSONSchema)e._zod.processJSONSchema(t,o.schema,r);else{let n=o.schema,a=t.processors[i.type];if(!a)throw Error(`[toJSONSchema]: Non-representable type encountered: ${i.type}`);a(e,t,n,r)}let a=e._zod.parent;a&&(o.ref||=a,U(a,t,r),t.seen.get(a).isParent=!0)}let c=t.metadataRegistry.get(e);return c&&Object.assign(o.schema,c),t.io===`input`&&W(e)&&(delete o.schema.examples,delete o.schema.default),t.io===`input`&&`_prefault`in o.schema&&((r=o.schema).default??(r.default=o.schema._prefault)),delete o.schema._prefault,t.seen.get(e).schema}function qi(e,t){let n=e.seen.get(t);if(!n)throw Error(`Unprocessed schema. This is a bug in Zod.`);let r=new Map;for(let t of e.seen.entries()){let n=e.metadataRegistry.get(t[0])?.id;if(n){let e=r.get(n);if(e&&e!==t[0])throw Error(`Duplicate schema id "${n}" detected during JSON Schema conversion. Two different schemas cannot share the same id when converted together.`);r.set(n,t[0])}}let i=t=>{let r=e.target===`draft-2020-12`?`$defs`:`definitions`;if(e.external){let n=e.external.registry.get(t[0])?.id,i=e.external.uri??(e=>e);if(n)return{ref:i(n)};let a=t[1].defId??t[1].schema.id??`schema${e.counter++}`;return t[1].defId=a,{defId:a,ref:`${i(`__shared`)}#/${r}/${a}`}}if(t[1]===n)return{ref:`#`};let i=`#/${r}/`,a=t[1].schema.id??`__schema${e.counter++}`;return{defId:a,ref:i+a}},a=e=>{if(e[1].schema.$ref)return;let t=e[1],{ref:n,defId:r}=i(e);t.def={...t.schema},r&&(t.defId=r);let a=t.schema;for(let e in a)delete a[e];a.$ref=n};if(e.cycles===`throw`)for(let t of e.seen.entries()){let e=t[1];if(e.cycle)throw Error(`Cycle detected: #/${e.cycle?.join(`/`)}/<root>

Set the \`cycles\` parameter to \`"ref"\` to resolve cyclical schemas with defs.`)}for(let n of e.seen.entries()){let r=n[1];if(t===n[0]){a(n);continue}if(e.external){let r=e.external.registry.get(n[0])?.id;if(t!==n[0]&&r){a(n);continue}}if(e.metadataRegistry.get(n[0])?.id){a(n);continue}if(r.cycle){a(n);continue}if(r.count>1&&e.reused===`ref`){a(n);continue}}}function Ji(e,t){let n=e.seen.get(t);if(!n)throw Error(`Unprocessed schema. This is a bug in Zod.`);let r=t=>{let n=e.seen.get(t);if(n.ref===null)return;let i=n.def??n.schema,a={...i},o=n.ref;if(n.ref=null,o){r(o);let n=e.seen.get(o),s=n.schema;if(s.$ref&&(e.target===`draft-07`||e.target===`draft-04`||e.target===`openapi-3.0`)?(i.allOf=i.allOf??[],i.allOf.push(s)):Object.assign(i,s),Object.assign(i,a),t._zod.parent===o)for(let e in i)e===`$ref`||e===`allOf`||e in a||delete i[e];if(s.$ref&&n.def)for(let e in i)e===`$ref`||e===`allOf`||e in n.def&&JSON.stringify(i[e])===JSON.stringify(n.def[e])&&delete i[e]}let s=t._zod.parent;if(s&&s!==o){r(s);let t=e.seen.get(s);if(t?.schema.$ref&&(i.$ref=t.schema.$ref,t.def))for(let e in i)e===`$ref`||e===`allOf`||e in t.def&&JSON.stringify(i[e])===JSON.stringify(t.def[e])&&delete i[e]}e.override({zodSchema:t,jsonSchema:i,path:n.path??[]})};for(let t of[...e.seen.entries()].reverse())r(t[0]);let i={};if(e.target===`draft-2020-12`?i.$schema=`https://json-schema.org/draft/2020-12/schema`:e.target===`draft-07`?i.$schema=`http://json-schema.org/draft-07/schema#`:e.target===`draft-04`?i.$schema=`http://json-schema.org/draft-04/schema#`:e.target,e.external?.uri){let n=e.external.registry.get(t)?.id;if(!n)throw Error("Schema is missing an `id` property");i.$id=e.external.uri(n)}Object.assign(i,n.def??n.schema);let a=e.metadataRegistry.get(t)?.id;a!==void 0&&i.id===a&&delete i.id;let o=e.external?.defs??{};for(let t of e.seen.entries()){let e=t[1];e.def&&e.defId&&(e.def.id===e.defId&&delete e.def.id,o[e.defId]=e.def)}e.external||Object.keys(o).length>0&&(e.target===`draft-2020-12`?i.$defs=o:i.definitions=o);try{let n=JSON.parse(JSON.stringify(i));return Object.defineProperty(n,"~standard",{value:{...t[`~standard`],jsonSchema:{input:Xi(t,`input`,e.processors),output:Xi(t,`output`,e.processors)}},enumerable:!1,writable:!1}),n}catch{throw Error(`Error converting schema to JSON.`)}}function W(e,t){let n=t??{seen:new Set};if(n.seen.has(e))return!1;n.seen.add(e);let r=e._zod.def;if(r.type===`transform`)return!0;if(r.type===`array`)return W(r.element,n);if(r.type===`set`)return W(r.valueType,n);if(r.type===`lazy`)return W(r.getter(),n);if(r.type===`promise`||r.type===`optional`||r.type===`nonoptional`||r.type===`nullable`||r.type===`readonly`||r.type==="default"||r.type===`prefault`)return W(r.innerType,n);if(r.type===`intersection`)return W(r.left,n)||W(r.right,n);if(r.type===`record`||r.type===`map`)return W(r.keyType,n)||W(r.valueType,n);if(r.type===`pipe`)return e._zod.traits.has(`$ZodCodec`)?!0:W(r.in,n)||W(r.out,n);if(r.type===`object`){for(let e in r.shape)if(W(r.shape[e],n))return!0;return!1}if(r.type===`union`){for(let e of r.options)if(W(e,n))return!0;return!1}if(r.type===`tuple`){for(let e of r.items)if(W(e,n))return!0;return!!(r.rest&&W(r.rest,n))}return!1}var Yi=(e,t={})=>n=>{let r=Ki({...n,processors:t});return U(e,r),qi(r,e),Ji(r,e)},Xi=(e,t,n={})=>r=>{let{libraryOptions:i,target:a}=r??{},o=Ki({...i??{},target:a,io:t,processors:n});return U(e,o),qi(o,e),Ji(o,e)},Zi={guid:`uuid`,url:`uri`,datetime:`date-time`,json_string:`json-string`,regex:``},Qi=(e,t,n,r)=>{let i=n;i.type=`string`;let{minimum:a,maximum:o,format:s,patterns:c,contentEncoding:l}=e._zod.bag;if(typeof a==`number`&&(i.minLength=a),typeof o==`number`&&(i.maxLength=o),s&&(i.format=Zi[s]??s,i.format===``&&delete i.format,s===`time`&&delete i.format),l&&(i.contentEncoding=l),c&&c.size>0){let e=[...c];e.length===1?i.pattern=e[0].source:e.length>1&&(i.allOf=[...e.map(e=>({...t.target===`draft-07`||t.target===`draft-04`||t.target===`openapi-3.0`?{type:`string`}:{},pattern:e.source}))])}},$i=(e,t,n,r)=>{let i=n,{minimum:a,maximum:o,format:s,multipleOf:c,exclusiveMaximum:l,exclusiveMinimum:u}=e._zod.bag;typeof s==`string`&&s.includes(`int`)?i.type=`integer`:i.type=`number`;let d=typeof u==`number`&&u>=(a??-1/0),f=typeof l==`number`&&l<=(o??1/0),p=t.target===`draft-04`||t.target===`openapi-3.0`;d?p?(i.minimum=u,i.exclusiveMinimum=!0):i.exclusiveMinimum=u:typeof a==`number`&&(i.minimum=a),f?p?(i.maximum=l,i.exclusiveMaximum=!0):i.exclusiveMaximum=l:typeof o==`number`&&(i.maximum=o),typeof c==`number`&&(i.multipleOf=c)},ea=(e,t,n,r)=>{n.type=`boolean`},ta=(e,t,n,r)=>{n.not={}},na=(e,t,n,r)=>{let i=e._zod.def,a=Fe(i.entries);a.every(e=>typeof e==`number`)&&(n.type=`number`),a.every(e=>typeof e==`string`)&&(n.type=`string`),n.enum=a},ra=(e,t,n,r)=>{if(t.unrepresentable===`throw`)throw Error(`Custom types cannot be represented in JSON Schema`)},ia=(e,t,n,r)=>{if(t.unrepresentable===`throw`)throw Error(`Transforms cannot be represented in JSON Schema`)},aa=(e,t,n,r)=>{let i=n,a=e._zod.def,{minimum:o,maximum:s}=e._zod.bag;typeof o==`number`&&(i.minItems=o),typeof s==`number`&&(i.maxItems=s),i.type=`array`,i.items=U(a.element,t,{...r,path:[...r.path,`items`]})},oa=(e,t,n,r)=>{let i=n,a=e._zod.def;i.type=`object`,i.properties={};let o=a.shape;for(let e in o)i.properties[e]=U(o[e],t,{...r,path:[...r.path,`properties`,e]});let s=new Set(Object.keys(o)),c=new Set([...s].filter(e=>{let n=a.shape[e]._zod;return t.io===`input`?n.optin===void 0:n.optout===void 0}));c.size>0&&(i.required=Array.from(c)),a.catchall?._zod.def.type===`never`?i.additionalProperties=!1:a.catchall?a.catchall&&(i.additionalProperties=U(a.catchall,t,{...r,path:[...r.path,`additionalProperties`]})):t.io===`output`&&(i.additionalProperties=!1)},sa=(e,t,n,r)=>{let i=e._zod.def,a=i.inclusive===!1,o=i.options.map((e,n)=>U(e,t,{...r,path:[...r.path,a?`oneOf`:`anyOf`,n]}));a?n.oneOf=o:n.anyOf=o},ca=(e,t,n,r)=>{let i=e._zod.def,a=U(i.left,t,{...r,path:[...r.path,`allOf`,0]}),o=U(i.right,t,{...r,path:[...r.path,`allOf`,1]}),s=e=>`allOf`in e&&Object.keys(e).length===1;n.allOf=[...s(a)?a.allOf:[a],...s(o)?o.allOf:[o]]},la=(e,t,n,r)=>{let i=n,a=e._zod.def;i.type=`object`;let o=a.keyType,s=o._zod.bag?.patterns;if(a.mode===`loose`&&s&&s.size>0){let e=U(a.valueType,t,{...r,path:[...r.path,`patternProperties`,`*`]});i.patternProperties={};for(let t of s)i.patternProperties[t.source]=e}else(t.target===`draft-07`||t.target===`draft-2020-12`)&&(i.propertyNames=U(a.keyType,t,{...r,path:[...r.path,`propertyNames`]})),i.additionalProperties=U(a.valueType,t,{...r,path:[...r.path,`additionalProperties`]});let c=o._zod.values;if(c){let e=[...c].filter(e=>typeof e==`string`||typeof e==`number`);e.length>0&&(i.required=e)}},ua=(e,t,n,r)=>{let i=e._zod.def,a=U(i.innerType,t,r),o=t.seen.get(e);t.target===`openapi-3.0`?(o.ref=i.innerType,n.nullable=!0):n.anyOf=[a,{type:`null`}]},da=(e,t,n,r)=>{let i=e._zod.def;U(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType},fa=(e,t,n,r)=>{let i=e._zod.def;U(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType,n.default=JSON.parse(JSON.stringify(i.defaultValue))},pa=(e,t,n,r)=>{let i=e._zod.def;U(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType,t.io===`input`&&(n._prefault=JSON.parse(JSON.stringify(i.defaultValue)))},ma=(e,t,n,r)=>{let i=e._zod.def;U(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType;let o;try{o=i.catchValue(void 0)}catch{throw Error(`Dynamic catch values are not supported in JSON Schema`)}n.default=o},ha=(e,t,n,r)=>{let i=e._zod.def,a=i.in._zod.traits.has(`$ZodTransform`),o=t.io===`input`?a?i.out:i.in:i.out;U(o,t,r);let s=t.seen.get(e);s.ref=o},ga=(e,t,n,r)=>{let i=e._zod.def;U(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType,n.readOnly=!0},_a=(e,t,n,r)=>{let i=e._zod.def;U(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType},va=L(`ZodISODateTime`,(e,t)=>{Un.init(e,t),K.init(e,t)});function ya(e){return pi(va,e)}var ba=L(`ZodISODate`,(e,t)=>{Wn.init(e,t),K.init(e,t)});function xa(e){return mi(ba,e)}var Sa=L(`ZodISOTime`,(e,t)=>{Gn.init(e,t),K.init(e,t)});function Ca(e){return hi(Sa,e)}var wa=L(`ZodISODuration`,(e,t)=>{Kn.init(e,t),K.init(e,t)});function Ta(e){return gi(wa,e)}var Ea=L(`ZodError`,(e,t)=>{_t.init(e,t),e.name=`ZodError`,Object.defineProperties(e,{format:{value:t=>bt(e,t)},flatten:{value:t=>yt(e,t)},addIssue:{value:t=>{e.issues.push(t),e.message=JSON.stringify(e.issues,Ie,2)}},addIssues:{value:t=>{e.issues.push(...t),e.message=JSON.stringify(e.issues,Ie,2)}},isEmpty:{get(){return e.issues.length===0}}})},{Parent:Error}),Da=xt(Ea),Oa=St(Ea),ka=Ct(Ea),Aa=Tt(Ea),ja=Dt(Ea),Ma=Ot(Ea),Na=kt(Ea),Pa=At(Ea),Fa=jt(Ea),Ia=Mt(Ea),La=Nt(Ea),Ra=Pt(Ea),za=new WeakMap;function Ba(e,t,n){let r=Object.getPrototypeOf(e),i=za.get(r);if(i||(i=new Set,za.set(r,i)),!i.has(t)){i.add(t);for(let e in n){let t=n[e];Object.defineProperty(r,e,{configurable:!0,enumerable:!1,get(){let n=t.bind(this);return Object.defineProperty(this,e,{configurable:!0,writable:!0,enumerable:!0,value:n}),n},set(t){Object.defineProperty(this,e,{configurable:!0,writable:!0,enumerable:!0,value:t})}})}}}var G=L(`ZodType`,(e,t)=>(V.init(e,t),Object.assign(e[`~standard`],{jsonSchema:{input:Xi(e,`input`),output:Xi(e,`output`)}}),e.toJSONSchema=Yi(e,{}),e.def=t,e.type=t.type,Object.defineProperty(e,"_def",{value:t}),e.parse=(t,n)=>Da(e,t,n,{callee:e.parse}),e.safeParse=(t,n)=>ka(e,t,n),e.parseAsync=async(t,n)=>Oa(e,t,n,{callee:e.parseAsync}),e.safeParseAsync=async(t,n)=>Aa(e,t,n),e.spa=e.safeParseAsync,e.encode=(t,n)=>ja(e,t,n),e.decode=(t,n)=>Ma(e,t,n),e.encodeAsync=async(t,n)=>Na(e,t,n),e.decodeAsync=async(t,n)=>Pa(e,t,n),e.safeEncode=(t,n)=>Fa(e,t,n),e.safeDecode=(t,n)=>Ia(e,t,n),e.safeEncodeAsync=async(t,n)=>La(e,t,n),e.safeDecodeAsync=async(t,n)=>Ra(e,t,n),Ba(e,`ZodType`,{check(...e){let t=this.def;return this.clone(Ue(t,{checks:[...t.checks??[],...e.map(e=>typeof e==`function`?{_zod:{check:e,def:{check:`custom`},onattach:[]}}:e)]}),{parent:!0})},with(...e){return this.check(...e)},clone(e,t){return $e(this,e,t)},brand(){return this},register(e,t){return e.add(this,t),this},refine(e,t){return this.check($o(e,t))},superRefine(e,t){return this.check(es(e,t))},overwrite(e){return this.check(Ii(e))},optional(){return Fo(this)},exactOptional(){return Lo(this)},nullable(){return zo(this)},nullish(){return Fo(zo(this))},nonoptional(e){return Go(this,e)},array(){return xo(this)},or(e){return To([this,e])},and(e){return Do(this,e)},transform(e){return Yo(this,No(e))},default(e){return Vo(this,e)},prefault(e){return Uo(this,e)},catch(e){return qo(this,e)},pipe(e){return Yo(this,e)},readonly(){return Zo(this)},describe(e){let t=this.clone();return Ur.add(t,{description:e}),t},meta(...e){if(e.length===0)return Ur.get(this);let t=this.clone();return Ur.add(t,e[0]),t},isOptional(){return this.safeParse(void 0).success},isNullable(){return this.safeParse(null).success},apply(e){return e(this)}}),Object.defineProperty(e,"description",{get(){return Ur.get(e)?.description},configurable:!0}),e)),Va=L(`_ZodString`,(e,t)=>{jn.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Qi(e,t,n,r);let n=e._zod.bag;e.format=n.format??null,e.minLength=n.minimum??null,e.maxLength=n.maximum??null,Ba(e,`_ZodString`,{regex(...e){return this.check(Ai(...e))},includes(...e){return this.check(Ni(...e))},startsWith(...e){return this.check(Pi(...e))},endsWith(...e){return this.check(Fi(...e))},min(...e){return this.check(Oi(...e))},max(...e){return this.check(Di(...e))},length(...e){return this.check(ki(...e))},nonempty(...e){return this.check(Oi(1,...e))},lowercase(e){return this.check(ji(e))},uppercase(e){return this.check(Mi(e))},trim(){return this.check(Ri())},normalize(...e){return this.check(Li(...e))},toLowerCase(){return this.check(zi())},toUpperCase(){return this.check(Bi())},slugify(){return this.check(Vi())}})}),Ha=L(`ZodString`,(e,t)=>{jn.init(e,t),Va.init(e,t),e.email=t=>e.check(Gr(Wa,t)),e.url=t=>e.check(Zr(qa,t)),e.jwt=t=>e.check(fi(co,t)),e.emoji=t=>e.check(Qr(Ja,t)),e.guid=t=>e.check(Kr(Ga,t)),e.uuid=t=>e.check(qr(Ka,t)),e.uuidv4=t=>e.check(Jr(Ka,t)),e.uuidv6=t=>e.check(Yr(Ka,t)),e.uuidv7=t=>e.check(Xr(Ka,t)),e.nanoid=t=>e.check($r(Ya,t)),e.guid=t=>e.check(Kr(Ga,t)),e.cuid=t=>e.check(ei(Xa,t)),e.cuid2=t=>e.check(ti(Za,t)),e.ulid=t=>e.check(ni(Qa,t)),e.base64=t=>e.check(li(ao,t)),e.base64url=t=>e.check(ui(oo,t)),e.xid=t=>e.check(ri($a,t)),e.ksuid=t=>e.check(ii(eo,t)),e.ipv4=t=>e.check(ai(to,t)),e.ipv6=t=>e.check(oi(no,t)),e.cidrv4=t=>e.check(si(ro,t)),e.cidrv6=t=>e.check(ci(io,t)),e.e164=t=>e.check(di(so,t)),e.datetime=t=>e.check(ya(t)),e.date=t=>e.check(xa(t)),e.time=t=>e.check(Ca(t)),e.duration=t=>e.check(Ta(t))});function Ua(e){return Wr(Ha,e)}var K=L(`ZodStringFormat`,(e,t)=>{H.init(e,t),Va.init(e,t)}),Wa=L(`ZodEmail`,(e,t)=>{Pn.init(e,t),K.init(e,t)}),Ga=L(`ZodGUID`,(e,t)=>{Mn.init(e,t),K.init(e,t)}),Ka=L(`ZodUUID`,(e,t)=>{Nn.init(e,t),K.init(e,t)}),qa=L(`ZodURL`,(e,t)=>{Fn.init(e,t),K.init(e,t)}),Ja=L(`ZodEmoji`,(e,t)=>{In.init(e,t),K.init(e,t)}),Ya=L(`ZodNanoID`,(e,t)=>{Ln.init(e,t),K.init(e,t)}),Xa=L(`ZodCUID`,(e,t)=>{Rn.init(e,t),K.init(e,t)}),Za=L(`ZodCUID2`,(e,t)=>{zn.init(e,t),K.init(e,t)}),Qa=L(`ZodULID`,(e,t)=>{Bn.init(e,t),K.init(e,t)}),$a=L(`ZodXID`,(e,t)=>{Vn.init(e,t),K.init(e,t)}),eo=L(`ZodKSUID`,(e,t)=>{Hn.init(e,t),K.init(e,t)}),to=L(`ZodIPv4`,(e,t)=>{qn.init(e,t),K.init(e,t)}),no=L(`ZodIPv6`,(e,t)=>{Jn.init(e,t),K.init(e,t)}),ro=L(`ZodCIDRv4`,(e,t)=>{Yn.init(e,t),K.init(e,t)}),io=L(`ZodCIDRv6`,(e,t)=>{Xn.init(e,t),K.init(e,t)}),ao=L(`ZodBase64`,(e,t)=>{Qn.init(e,t),K.init(e,t)}),oo=L(`ZodBase64URL`,(e,t)=>{er.init(e,t),K.init(e,t)}),so=L(`ZodE164`,(e,t)=>{tr.init(e,t),K.init(e,t)}),co=L(`ZodJWT`,(e,t)=>{rr.init(e,t),K.init(e,t)}),lo=L(`ZodNumber`,(e,t)=>{ir.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>$i(e,t,n,r),Ba(e,`ZodNumber`,{gt(e,t){return this.check(wi(e,t))},gte(e,t){return this.check(Ti(e,t))},min(e,t){return this.check(Ti(e,t))},lt(e,t){return this.check(Si(e,t))},lte(e,t){return this.check(Ci(e,t))},max(e,t){return this.check(Ci(e,t))},int(e){return this.check(po(e))},safe(e){return this.check(po(e))},positive(e){return this.check(wi(0,e))},nonnegative(e){return this.check(Ti(0,e))},negative(e){return this.check(Si(0,e))},nonpositive(e){return this.check(Ci(0,e))},multipleOf(e,t){return this.check(Ei(e,t))},step(e,t){return this.check(Ei(e,t))},finite(){return this}});let n=e._zod.bag;e.minValue=Math.max(n.minimum??-1/0,n.exclusiveMinimum??-1/0)??null,e.maxValue=Math.min(n.maximum??1/0,n.exclusiveMaximum??1/0)??null,e.isInt=(n.format??``).includes(`int`)||Number.isSafeInteger(n.multipleOf??.5),e.isFinite=!0,e.format=n.format??null});function uo(e){return _i(lo,e)}var fo=L(`ZodNumberFormat`,(e,t)=>{ar.init(e,t),lo.init(e,t)});function po(e){return vi(fo,e)}var mo=L(`ZodBoolean`,(e,t)=>{or.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ea(e,t,n,r)});function ho(e){return yi(mo,e)}var go=L(`ZodUnknown`,(e,t)=>{sr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(e,t,n)=>void 0});function _o(){return bi(go)}var vo=L(`ZodNever`,(e,t)=>{cr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ta(e,t,n,r)});function yo(e){return xi(vo,e)}var bo=L(`ZodArray`,(e,t)=>{ur.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>aa(e,t,n,r),e.element=t.element,Ba(e,`ZodArray`,{min(e,t){return this.check(Oi(e,t))},nonempty(e){return this.check(Oi(1,e))},max(e,t){return this.check(Di(e,t))},length(e,t){return this.check(ki(e,t))},unwrap(){return this.element}})});function xo(e,t){return Hi(bo,e,t)}var So=L(`ZodObject`,(e,t)=>{hr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>oa(e,t,n,r),R(e,`shape`,()=>t.shape),Ba(e,`ZodObject`,{keyof(){return jo(Object.keys(this._zod.def.shape))},catchall(e){return this.clone({...this._zod.def,catchall:e})},passthrough(){return this.clone({...this._zod.def,catchall:_o()})},loose(){return this.clone({...this._zod.def,catchall:_o()})},strict(){return this.clone({...this._zod.def,catchall:yo()})},strip(){return this.clone({...this._zod.def,catchall:void 0})},extend(e){return it(this,e)},safeExtend(e){return at(this,e)},merge(e){return ot(this,e)},pick(e){return nt(this,e)},omit(e){return rt(this,e)},partial(...e){return st(Po,this,e[0])},required(...e){return ct(Wo,this,e[0])}})});function Co(e,t){return new So({type:`object`,shape:e??{},...z(t)})}var wo=L(`ZodUnion`,(e,t)=>{_r.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>sa(e,t,n,r),e.options=t.options});function To(e,t){return new wo({type:`union`,options:e,...z(t)})}var Eo=L(`ZodIntersection`,(e,t)=>{vr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ca(e,t,n,r)});function Do(e,t){return new Eo({type:`intersection`,left:e,right:t})}var Oo=L(`ZodRecord`,(e,t)=>{xr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>la(e,t,n,r),e.keyType=t.keyType,e.valueType=t.valueType});function ko(e,t,n){return!t||!t._zod?new Oo({type:`record`,keyType:Ua(),valueType:e,...z(t)}):new Oo({type:`record`,keyType:e,valueType:t,...z(n)})}var Ao=L(`ZodEnum`,(e,t)=>{Sr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>na(e,t,n,r),e.enum=t.entries,e.options=Object.values(t.entries);let n=new Set(Object.keys(t.entries));e.extract=(e,r)=>{let i={};for(let r of e)if(n.has(r))i[r]=t.entries[r];else throw Error(`Key ${r} not found in enum`);return new Ao({...t,checks:[],...z(r),entries:i})},e.exclude=(e,r)=>{let i={...t.entries};for(let t of e)if(n.has(t))delete i[t];else throw Error(`Key ${t} not found in enum`);return new Ao({...t,checks:[],...z(r),entries:i})}});function jo(e,t){return new Ao({type:`enum`,entries:Array.isArray(e)?Object.fromEntries(e.map(e=>[e,e])):e,...z(t)})}var Mo=L(`ZodTransform`,(e,t)=>{Cr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ia(e,t,n,r),e._zod.parse=(n,r)=>{if(r.direction===`backward`)throw new Me(e.constructor.name);n.addIssue=r=>{if(typeof r==`string`)n.issues.push(ht(r,n.value,t));else{let t=r;t.fatal&&(t.continue=!1),t.code??=`custom`,t.input??=n.value,t.inst??=e,n.issues.push(ht(t))}};let i=t.transform(n.value,n);return i instanceof Promise?i.then(e=>(n.value=e,n.fallback=!0,n)):(n.value=i,n.fallback=!0,n)}});function No(e){return new Mo({type:`transform`,transform:e})}var Po=L(`ZodOptional`,(e,t)=>{Tr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>_a(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function Fo(e){return new Po({type:`optional`,innerType:e})}var Io=L(`ZodExactOptional`,(e,t)=>{Er.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>_a(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function Lo(e){return new Io({type:`optional`,innerType:e})}var Ro=L(`ZodNullable`,(e,t)=>{Dr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ua(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function zo(e){return new Ro({type:`nullable`,innerType:e})}var Bo=L(`ZodDefault`,(e,t)=>{Or.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>fa(e,t,n,r),e.unwrap=()=>e._zod.def.innerType,e.removeDefault=e.unwrap});function Vo(e,t){return new Bo({type:`default`,innerType:e,get defaultValue(){return typeof t==`function`?t():Xe(t)}})}var Ho=L(`ZodPrefault`,(e,t)=>{Ar.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>pa(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function Uo(e,t){return new Ho({type:`prefault`,innerType:e,get defaultValue(){return typeof t==`function`?t():Xe(t)}})}var Wo=L(`ZodNonOptional`,(e,t)=>{jr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>da(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function Go(e,t){return new Wo({type:`nonoptional`,innerType:e,...z(t)})}var Ko=L(`ZodCatch`,(e,t)=>{Nr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ma(e,t,n,r),e.unwrap=()=>e._zod.def.innerType,e.removeCatch=e.unwrap});function qo(e,t){return new Ko({type:`catch`,innerType:e,catchValue:typeof t==`function`?t:()=>t})}var Jo=L(`ZodPipe`,(e,t)=>{Pr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ha(e,t,n,r),e.in=t.in,e.out=t.out});function Yo(e,t){return new Jo({type:`pipe`,in:e,out:t})}var Xo=L(`ZodReadonly`,(e,t)=>{Ir.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ga(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function Zo(e){return new Xo({type:`readonly`,innerType:e})}var Qo=L(`ZodCustom`,(e,t)=>{Rr.init(e,t),G.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ra(e,t,n,r)});function $o(e,t={}){return Ui(Qo,e,t)}function es(e,t){return Wi(e,t)}var ts=Ua().uuid(),ns=uo().int().nonnegative().nullable(),rs=ko(Ua(),_o()),is=Co({stream_epoch:ts,retained_from:ns,newest:ns,replay_gap:ho(),epoch_mismatch:ho(),requires_reconciliation:ho()}).passthrough(),as=Co({session_id:ts,run_id:ts,agent_id:ts,has_active_run:ho(),ts:Ua().min(1)}).passthrough(),os=Co({sessions:xo(Co({id:ts,session_type:Ua().min(1),has_active_run:ho()}).passthrough())}).passthrough(),ss=Co({agents:xo(Co({id:ts,name:Ua().min(1),is_default:ho()}).passthrough())}).passthrough(),cs=new Map([[`stream_state`,is],[`session_activity_started`,as],[`session_activity_ended`,as]]),ls=class extends Error{boundary;issues;constructor(e,t){let n=t.slice(0,3).map(e=>`${e.path.join(`.`)||`<root>`}: ${e.message}`).join(`; `);super(`Invalid ${e} payload: ${n}`),this.boundary=e,this.issues=t,this.name=`ContractViolation`}};function us(e,t,n){let r=t.safeParse(n);if(!r.success)throw new ls(e,r.error.issues);return r.data}function ds(e,t){let n=cs.get(e)??rs;return us(`SSE ${e}`,n,t)}function fs(e,t,n){let r=new URL(e,`http://alms.local`).pathname,i=t.toUpperCase();return i===`GET`&&r===`/sessions`?us(`GET /sessions`,os,n):i===`GET`&&r===`/agents`?us(`GET /agents`,ss,n):us(`${i} ${r}`,rs,n)}function ps(e){try{return e()}catch(e){let t=e instanceof ls?e:new ls(`unknown`,[{code:`custom`,path:[],message:e instanceof Error?e.message:String(e),input:e}]);throw console.error(`[contract-boundary]`,t),ke(t.message),t}}function ms(){let e={version:1,parseApiResponse:(e,t,n)=>ps(()=>fs(e,t,n)),parseSsePayload:(e,t)=>ps(()=>ds(e,t))};return globalThis.__almsContracts=e,e}ms();var hs=()=>v(`/settings`),gs=e=>se(`/settings`,e),_s=(e,t)=>{let n=new URLSearchParams;e&&n.set(`agent_id`,e),t&&t.includeDms&&n.set(`include_dms`,`true`);let r=n.toString();return v(`/sessions${r?`?`+r:``}`)},vs=(e,t)=>m(`/sessions`,{agent_id:e,context_id:t}),ys=e=>v(`/sessions/${e}/messages`),bs=e=>v(`/session/${e}`),xs=e=>g(`/sessions/${e}`),Ss=e=>v(`/sessions/${e}/tool-calls`),Cs=e=>m(`/sessions/${e}/cancel-dm`),ws=e=>m(`/sessions/${e}/subagent/cancel`),Ts=d({});async function Es(){try{Ts.value=await hs()}catch(e){console.error(`[settings] refresh failed:`,e)}}var Ds=d(null),Os=d(null),ks=e({agentSwitchLoading:()=>js,bootRetryAvailable:()=>Ms,runBoot:()=>Fs,sessionSwitchLoading:()=>As,setRunBoot:()=>Ps}),As=d(!1),js=d(!1),Ms=d(!1),Ns=null;function Ps(e){Ns=e}function Fs(){Ns&&Ns()}var Is=4e3,Ls=d(null),Rs=null;function zs(){Rs&&=(clearTimeout(Rs),null)}function Bs(e){return!!(e&&e.status===`running`&&e.sessionId)}function Vs(e){return!!e&&Ls.value===e}function Hs(e){return e?(zs(),Ls.value=e,Rs=setTimeout(()=>{Rs=null,Ls.value=null},Is),!0):!1}function Us(){zs(),Ls.value=null}function Ws(e){e&&Ls.value===e&&Us()}async function Gs(e){if(!Vs(e))return!1;Us();try{return await ws(e),!0}catch(t){return console.error(`[confirmSubagentCancel] cancel failed for session`,e,t),!1}}var Ks=`modulepreload`,qs=function(e){return`/ui/`+e},Js={},Ys=function(e,t,n){let r=Promise.resolve();if(t&&t.length>0){let e=document.getElementsByTagName(`link`),i=document.querySelector(`meta[property=csp-nonce]`),a=i?.nonce||i?.getAttribute(`nonce`);function o(e){return Promise.all(e.map(e=>Promise.resolve(e).then(e=>({status:`fulfilled`,value:e}),e=>({status:`rejected`,reason:e}))))}function s(e){return import.meta.resolve?import.meta.resolve(e):new URL(e,import.meta.url).href}r=o(t.map(t=>{if(t=qs(t,n),t=s(t),t in Js)return;Js[t]=!0;let r=t.endsWith(`.css`);for(let n=e.length-1;n>=0;n--){let i=e[n];if(i.href===t&&(!r||i.rel===`stylesheet`))return}let i=document.createElement(`link`);if(i.rel=r?`stylesheet`:Ks,r||(i.as=`script`),i.crossOrigin=``,i.href=t,a&&i.setAttribute(`nonce`,a),document.head.appendChild(i),r)return new Promise((e,n)=>{i.addEventListener(`load`,e),i.addEventListener(`error`,()=>n(Error(`Unable to preload CSS for ${t}`)))})}))}function i(e){let t=new Event(`vite:preloadError`,{cancelable:!0});if(t.payload=e,window.dispatchEvent(t),!t.defaultPrevented)throw e}return r.then(t=>{for(let e of t||[])e.status===`rejected`&&i(e.reason);return e().catch(i)})},q=d({}),J=new Map,Xs=8,Zs=3e4;function Qs(e,t,n,r,i){for(J.delete(e),J.set(e,{kind:t,tool:n||null,toolInvocationId:r||null,parentToolInvocationId:i||null,updatedAt:Date.now()});J.size>Xs;){let e=J.keys().next().value;J.delete(e)}}function $s(e){let t=J.get(e);return!t||(J.delete(e),Date.now()-t.updatedAt>Zs)?null:{kind:t.kind,tool:t.tool,toolInvocationId:t.toolInvocationId||null,parentToolInvocationId:t.parentToolInvocationId||null}}function ec(e,t){if(t){for(let[e,n]of J)if(n.parentToolInvocationId===t)return $s(e)}if(!J.get(e)?.parentToolInvocationId){let t=$s(e);if(t)return t}if(!e.startsWith(`subagent-`))return null;for(let e of[...J.keys()]){if(!e.startsWith(`subagent-`)||J.get(e)?.parentToolInvocationId)continue;let t=$s(e);if(t)return t}return null}var tc=d(null),Y={},nc=15e3,rc=new Set([`done`,`fail`,`cancelled`]);function ic(e){Y[e]&&clearTimeout(Y[e]),Y[e]=setTimeout(()=>{delete Y[e];let{[e]:t,...n}=q.value;t&&Ws(t.sessionId),q.value=n},nc)}function ac(){for(let[e,t]of Object.entries(q.value))rc.has(t.status)&&!Y[e]&&ic(e)}function oc(e){if(!e)return{activity:null,toolsUsed:0,countedToolIds:new Set};let t=e.kind===`tool_start`;return{activity:{kind:e.kind,tool:e.tool},toolsUsed:+!!t,countedToolIds:t&&e.toolInvocationId?new Set([e.toolInvocationId]):new Set}}function sc(e,t,n){let r=e===`subagent`&&n?`subagent-`+n.slice(0,8):e;Y[r]&&(clearTimeout(Y[r]),delete Y[r]);let i=oc(ec(r,n||null));q.value={...q.value,[r]:{status:`running`,task:t||``,toolInvocationId:n||null,displayName:e,startedAt:Date.now(),sessionId:null,activity:i.activity,toolsUsed:i.toolsUsed,countedToolIds:i.countedToolIds}}}var cc=Symbol(`drop-stale-subagent-signal`);function lc(e,t){if(t){let n=pc(t);if(n)return q.value[n]?.status===`running`?uc(n,e):cc;let r=q.value[e];return r?r.toolInvocationId?cc:e:null}if(q.value[e])return e;if(e.startsWith(`subagent-`)){for(let[t,n]of Object.entries(q.value))if(t.startsWith(`subagent-`)&&n.status===`running`)return uc(t,e)}return null}function uc(e,t){if(e===t||!t.startsWith(`subagent-`))return e;let{[e]:n,...r}=q.value;return q.value={...r,[t]:n},Y[e]&&(clearTimeout(Y[e]),delete Y[e]),t}function dc(e,t,n,r,i){if(!t)return;let a=lc(e,i);if(a===cc)return;if(!a){Qs(e,t,n,r,i);return}let o=q.value[a];if(!o)return;J.delete(e);let s=o.countedToolIds instanceof Set?o.countedToolIds:new Set,c=o.toolsUsed||0,l=s;t===`tool_start`&&(r?s.has(r)||(l=new Set(s),l.add(r),c+=1):c+=1),q.value={...q.value,[a]:{...o,activity:{kind:t,tool:n||null},toolsUsed:c,countedToolIds:l}}}function fc(e,t,n,r){J.delete(e),Ws(r);let i=hc(e,n,r);if(!i)return;J.delete(i);let a=q.value[i];a&&(Ws(a.sessionId),q.value={...q.value,[i]:{...a,status:t}},ic(i))}function pc(e){if(!e)return null;for(let[t,n]of Object.entries(q.value))if(n.toolInvocationId===e)return t;return null}function mc(e){if(!e)return null;for(let[t,n]of Object.entries(q.value))if(n.sessionId===e)return t;return null}function hc(e,t,n){let r=pc(t);if(r)return r;let i=mc(n);if(i)return i;if(q.value[e])return e;if(e===`subagent`){for(let[e,t]of Object.entries(q.value))if(e.startsWith(`subagent-`)&&t.status===`running`)return e}return null}function gc(e,t,n){let r=hc(e,n,t);if(!r)return;let i=q.value[r];i&&(q.value={...q.value,[r]:{...i,sessionId:t}})}async function _c(){let[e,t,n,r,i,a,o,s]=await Promise.all([Ys(()=>Promise.resolve().then(()=>Ul),void 0),Ys(()=>Promise.resolve().then(()=>Kc),void 0),Ys(()=>import(`./chat-actions-UbdvPXnD.js`).then(e=>e.n),__vite__mapDeps([0,1,2])),Ys(()=>import(`./runs-D_HMln2i.js`).then(e=>e.a),__vite__mapDeps([3,1,2])),Ys(()=>import(`./select-generation-DvILpFQd.js`).then(e=>e.r),__vite__mapDeps([4,1])),Ys(()=>Promise.resolve().then(()=>ks),void 0),Ys(()=>Promise.resolve().then(()=>iu),void 0),Ys(()=>import(`./agents-Dxvrmzg8.js`).then(e=>e.i),__vite__mapDeps([5,1,2]))]);return{loadSession:e.loadSession,closeSessionStream:t.closeSessionStream,replaceMessages:n.replaceMessages,activeRunId:r.activeRunId,selectedRunId:r.selectedRunId,bumpSelectGeneration:i.bumpSelectGeneration,selectGeneration:i,sessionSwitchLoading:a.sessionSwitchLoading,saveActiveSession:o.saveActiveSession,activeAgentId:s.activeAgentId}}async function vc(e,t){let n=await _c(),r=n.bumpSelectGeneration();n.closeSessionStream(),x.value=e,n.activeRunId.value=null,n.selectedRunId.value=null,n.replaceMessages([]),xc(),n.activeAgentId.value&&n.saveActiveSession(n.activeAgentId.value,e),n.sessionSwitchLoading.value=!0;try{await n.loadSession(e,{isStale:()=>r!==n.selectGeneration.selectGeneration,logPrefix:t})}finally{r===n.selectGeneration.selectGeneration&&(n.sessionSwitchLoading.value=!1)}}function yc(e){if(!e)return;let t=x.value;t&&(tc.value=t),vc(e,`navigateToSubagent`).catch(e=>{console.error(`[navigateToSubagentSession] failed:`,e)})}function bc(){let e=tc.value;e&&(tc.value=null,vc(e,`navigateToParent`).catch(e=>{console.error(`[navigateToParentSession] failed:`,e)}))}function xc(){for(let e of Object.keys(Y))clearTimeout(Y[e]),delete Y[e];J.clear(),Us(),q.value={}}function Sc(e){if(ac(),!Array.isArray(e)||e.length===0)return;let t=new Map,n=[],r={},i=new Map,a=-1/0,o=!1;for(let r of e){if(!o&&r&&typeof r.ts==`string`){let e=Date.parse(r.ts);Number.isFinite(e)&&(e<a?(console.warn(`[rehydrateSubagentsFromHistory] messages are not in chronological order; FIFO pairing of subagent invocations to completion markers may be wrong. See PR #1049 / Tim review suggestion 2.`),o=!0):a=e)}if(r.type===`subagent_started`){r.toolInvocationId&&r.subagentSessionId&&i.set(r.toolInvocationId,r.subagentSessionId);continue}if(r.type===`subagent_completed`){if(!r.sessionId)continue;let e=t.get(r.sessionId);if(e&&e.length>0){let t=e.shift();t.paired=!0}continue}if(r.type!==`tool`||r.tool!==`invoke_agent`)continue;let e=typeof r.result==`object`&&r.result||null,s=!!(e&&e.task_id),c=e?.session_id||null;if(s){let e={msg:r,paired:!1};if(n.push(e),c){let n=t.get(c);n||(n=[],t.set(c,n)),n.push(e)}continue}r.status===`running`&&n.push({msg:r,paired:!1})}for(let e of n){if(e.paired)continue;let t=e.msg,n=t.params||{},a=typeof t.result==`object`&&t.result||null,o=n.name||n.subagent_name||`subagent`,s=n.task||``,c=t.id||null,l=a?.session_id||c&&i.get(c)||null,u=o===`subagent`&&c?`subagent-`+String(c).slice(0,8):o;if(q.value[u]){!q.value[u].sessionId&&l&&gc(o,l,c);continue}let d=t.ts&&Date.parse(t.ts)||Date.now(),f=oc(ec(u,c||null));r[u]={status:`running`,task:s,toolInvocationId:c,displayName:o,startedAt:d,sessionId:l,activity:f.activity,toolsUsed:f.toolsUsed,countedToolIds:f.countedToolIds}}Object.keys(r).length!==0&&(q.value={...q.value,...r})}var Cc=d({phase:null,detail:null}),X=d(null);function wc(e,t){if(!e)return null;switch(e){case`building_context`:return`Building context…`;case`summarizing`:return`Summarizing history…`;case`calling_llm`:return`Thinking…`;case`executing_tools`:return t?`Running ${t}\u2026`:`Running tools…`;case`tool_active`:return t?`Running ${t}\u2026`:`Running tool…`;case`dm`:return t?`Chatting with ${t}\u2026`:`In conversation…`;default:return null}}a(()=>{let{phase:e,detail:t}=Cc.value;return wc(e,t)});function Tc(e,t){Cc.value={phase:e,detail:t||null}}function Ec(){Cc.value={phase:null,detail:null},X.value=null}function Dc(e){X.value=e,Cc.value={phase:`dm`,detail:e}}function Oc(e){X.value?Cc.value={phase:`dm`,detail:X.value}:e&&(Cc.value={phase:e,detail:null})}function kc(e){return{approvalId:e.approval_id,tool:e.tool||e.capability,params:e.params||e.request,runId:e.run_id||null}}var Ac={ignored:`no further replies`,depth_exceeded:`message limit reached`,user_cancelled:`cancelled by user`,errored:`run failed`},jc=d(!1),Mc=new Set,Nc=()=>{},Pc=()=>{};function Fc(e){if(!e||Mc.has(e))return;let t=Mc.size===0;Mc.add(e),t&&(jc.value=!0)}function Ic(e){e&&Mc.has(e)&&(Mc.delete(e),Mc.size===0&&(jc.value=!1))}function Lc(e){Nc=typeof e==`function`?e:()=>{}}function Rc(e){Pc=typeof e==`function`?e:()=>{}}function zc(){try{Nc()}catch(e){console.error(`[stream-health] session reconnect failed:`,e)}try{Pc()}catch(e){console.error(`[stream-health] agent-events reconnect failed:`,e)}}function Bc(){zc()}function Vc(){if(typeof window>`u`)return()=>{};let e=()=>Bc();return window.addEventListener(`online`,e),()=>window.removeEventListener(`online`,e)}var Hc=new Map;function Uc(e,t){!e||!(t instanceof Set)||Hc.set(e,t)}function Wc(e){return e&&Hc.get(e)||null}function Gc(e){if(e==null){Hc.clear();return}Hc.delete(e)}var Kc=e({closeSessionStream:()=>gl,dmThinkingBuffers:()=>qc,isSessionStreamOpen:()=>_l,openSessionStream:()=>ml}),qc=d(new Map),Jc=new Map,Yc=new Set;function Xc(e){if(e&&e.startsWith(`peer:`)){let t=e.slice(5),n=S.value;return n.length>=2?n[0]===t?n[1]:n[0]:null}return A.value?.name||null}function Zc(e){return C.value?.session_type===`dm`?!0:e?Yc.has(e)?!0:F.value.some(t=>t.type===`dm_reasoning`&&t.runId===e):!1}function Qc(e,t){switch(e){case`AUTH`:return`Authentication failed -- check your API key in Settings.`;case`RATE_LIMIT`:return`Rate limited by the LLM provider -- wait a moment and try again.`;case`TIMEOUT`:return`Request timed out -- the LLM provider did not respond in time.`;default:return t}}var $c=null,el=null,tl=0,nl=10,rl=null,il=``,al=null,ol=-1,sl=!1,cl=null;function ll(){if(al=null,!il)return;let e=il;il=``,N(t=>{let n=[...t.filter(e=>e.type!==`thinking`)],r=n[n.length-1];r&&r.type===`agent`&&!r.sealed?n[n.length-1]={...r,text:r.text+e}:n.push({id:P(),type:`agent`,role:`assistant`,text:e,sealed:!1,ts:new Date().toISOString()});let i=t.filter(e=>e.type===`tool`).length,a=n.filter(e=>e.type===`tool`).length;return a<i&&console.warn(`[flushDeltaBuffer] tool message count decreased:`,i,`->`,a),n})}function ul(){if(al===null){let e=ol;al=requestAnimationFrame(()=>{if(e!==Ce){al=null,il=``;return}c(ll)})}}function dl(e){if(!e)return;let t=Jc.get(e);if(!t)return;Jc.delete(e);let n=qc.value,r=new Map(n);r.set(e,(r.get(e)||``)+t),qc.value=r}function fl(e){e&&Jc.delete(e)}function pl(){let e=F.value,t=e.some(e=>e.type===`thinking`),n=t?e.filter(e=>e.type!==`thinking`):e,r=n[n.length-1];r&&r.type===`agent`&&!r.sealed?N(()=>{let t=[...n];t[t.length-1]={...r,sealed:!0};let i=e.filter(e=>e.type===`tool`).length,a=t.filter(e=>e.type===`tool`).length;return a<i&&console.warn(`[sealLastAgent] tool message count decreased:`,i,`->`,a),t}):t&&N(()=>{let t=e.filter(e=>e.type===`tool`).length,r=n.filter(e=>e.type===`tool`).length;return r<t&&console.warn(`[sealLastAgent] tool message count decreased:`,t,`->`,r),n})}function ml(e,t){let n=e&&(!t||!(t.sealedReasoningRunIds instanceof Set))?Wc(e):null,r=e&&e===el&&(!t||!(t.sealedReasoningRunIds instanceof Set))?new Map(Jc):null,i=e&&e===el&&(!t||!(t.sealedReasoningRunIds instanceof Set))?new Set(Yc):null;if(gl(),!e)return;rl!==null&&(clearTimeout(rl),rl=null);let a=localStorage.getItem(`alms_auth_token`),o=new URLSearchParams;a&&o.set(`token`,a),t&&t.lastEventId!=null&&o.set(`last_event_id`,String(t.lastEventId));let s=o.toString(),l=`/sessions/${e}/events${s?`?`+s:``}`,u=new EventSource(l);$c=u,el=e,tl=0,cl=t&&t.lastEventId!=null?t.lastEventId:null,t&&t.sealedReasoningRunIds instanceof Set?Uc(e,t.sealedReasoningRunIds):n instanceof Set&&Uc(e,n);let d=Wc(e);if(r instanceof Map)for(let[e,t]of r)Jc.set(e,t);if(i instanceof Set)for(let e of i)Yc.add(e);u.addEventListener(`open`,()=>{u===$c&&Ic(`session`)}),ol=Ce;let f=new Set,p=(e,t)=>u.addEventListener(e,n=>{if(ol!==Ce)return;let r=n.lastEventId;if(r&&/^\d+$/.test(r)&&(cl=r),r&&!r.startsWith(`ephemeral-`)){if(f.has(r))return;if(f.add(r),f.size>2500){let e=0;for(let t of f){if(e++>=500)break;f.delete(t)}console.debug(`[sse-dedup] evicted`,500,`stale IDs, size:`,f.size)}}let i=JSON.parse(n.data),a=globalThis.__almsContracts,o=a?a.parseSsePayload(e,i):i;t({data:JSON.stringify(o),lastEventId:n.lastEventId})});p(`run_created`,e=>{let t=JSON.parse(e.data),n=t.queued_behind||0;sl=!1,_e();let r=C.value?.session_type===`dm`,i=!!(t.source&&t.source.startsWith(`peer:`));if(i){Dc(t.source.slice(5));let e=t.run_id||M.value;e&&Yc.add(e)}if((r||i)&&t.run_id)if(n>0)c(()=>{M.value=t.run_id,I({id:P(),type:`thinking`,source:t.source,queuedBehind:n,runId:t.run_id})});else{let e=Xc(t.source);c(()=>{M.value=t.run_id,I({id:P(),type:`dm_reasoning`,runId:t.run_id,agentName:e,thinkingText:``,tools:[],status:`running`,isLive:!0})})}else t.is_notification?c(()=>{M.value=t.run_id,I({id:P(),type:`thinking`,source:t.source,queuedBehind:n,runId:t.run_id})}):c(n>0?()=>{M.value=t.run_id,xe(e=>e.type===`thinking`&&e.pending,e=>({...e,queuedBehind:n,pending:!1,runId:t.run_id}))}:()=>{M.value=t.run_id,xe(e=>e.type===`thinking`&&e.pending,e=>({...e,pending:!1}))})}),p(`run_started`,e=>{let t=JSON.parse(e.data);if(Zc(t.run_id)&&t.run_id){let e=Xc(F.value.find(e=>e.type===`thinking`&&e.queuedBehind>0)?.source);N(n=>{let r=n.filter(e=>!(e.type===`thinking`&&e.queuedBehind>0));return r.some(e=>e.type===`dm_reasoning`&&e.runId===t.run_id)||r.push({id:P(),type:`dm_reasoning`,runId:t.run_id,agentName:e,thinkingText:``,tools:[],status:`running`,isLive:!0}),r})}else xe(e=>e.type===`thinking`&&e.queuedBehind>0,e=>({...e,queuedBehind:0}));X.value?Tc(`dm`,X.value):Tc(`calling_llm`,null),_e()}),p(`run_queue_position`,e=>{let t=JSON.parse(e.data),n=typeof t.position==`number`?t.position:0;n<=0||xe(e=>e.type===`thinking`&&e.queuedBehind>0&&(e.runId===t.run_id||!e.runId),e=>({...e,queuedBehind:n}))}),p(`status`,e=>{let t=JSON.parse(e.data);if(console.debug(`[status]`,t.phase,t.detail||``),X.value){Tc(`dm`,X.value);return}Tc(t.phase,t.detail||null)}),p(`token_delta`,e=>{let t=JSON.parse(e.data);if(!t.source_agent){if(Zc(t.run_id||M.value)){let e=t.run_id||M.value;e&&Jc.set(e,(Jc.get(e)||``)+t.delta);return}sl=!0,il+=t.delta,ul()}}),p(`reasoning_delta`,e=>{let t=JSON.parse(e.data);if(t.source_agent)return;let n=t.text||``;if(n){if(Zc(t.run_id||M.value)){let e=t.run_id||M.value;if(e){let t=qc.value,r=new Map(t);r.set(e,(r.get(e)||``)+n),qc.value=r}return}t.run_id&&d&&d.has(t.run_id)||N(e=>{let t=[...e.filter(e=>e.type!==`thinking`)],r=t[t.length-1];return r&&r.type===`agent`&&!r.sealed?t[t.length-1]={...r,reasoning:(r.reasoning||``)+n}:t.push({id:P(),type:`agent`,role:`assistant`,text:``,reasoning:n,sealed:!1,ts:new Date().toISOString()}),t})}}),p(`stream_reset`,e=>{let t=JSON.parse(e.data);if(t.source_agent)return;let n=t.run_id||M.value||null;c(()=>{if(n&&Jc.delete(n),n&&qc.value.has(n)){let e=new Map(qc.value);e.delete(n),qc.value=e}il=``,N(e=>{let t=[...e],n=t[t.length-1];return n&&n.type===`agent`&&!n.sealed?(t.pop(),t):e})})}),p(`tool_start`,e=>{c(()=>{ll();let t=JSON.parse(e.data),n=t.tool_invocation_id||t.call_id||P(),r=t.run_id||M.value||null,i=F.value.filter(e=>e.type===`tool`).length;console.debug(`[tool_start]`,t.tool,`id=`+n,`tool count before insertion:`,i);let a=Date.now();if(Zc(r)&&!t.source_agent){dl(r);let e={id:n,type:`tool`,tool:t.tool,params:t.params,status:`running`,startedAt:a,runId:r};N(t=>{let n=t.findIndex(e=>e.type===`dm_reasoning`&&e.runId===r);if(n>=0){let r=t[n],i=[...t];return i[n]={...r,tools:[...r.tools,e]},i}return[...t,{id:P(),type:`dm_reasoning`,runId:r,agentName:null,thinkingText:``,tools:[e],status:`running`,isLive:!0}]})}else if(t.tool===`invoke_agent`){pl();let e=t.params?.name||t.params?.subagent_name||`subagent`,i=t.params?.task||``;I({id:n,type:`tool`,tool:`invoke_agent`,params:t.params,status:`running`,startedAt:a,runId:r}),sc(e,i,n),t.subagent_session_id&&gc(pc(n)||e,t.subagent_session_id)}else t.source_agent||(pl(),I({id:n,type:`tool`,tool:t.tool,params:t.params,status:`running`,startedAt:a,runId:r}),X.value||Tc(`tool_active`,t.tool))})}),p(`tool_end`,e=>{c(()=>{let t=JSON.parse(e.data),n=t.tool_invocation_id,r=t.ok?`done`:`fail`;if(t.source_agent)return;let i=Date.now(),a=e=>{let n=e.startedAt?i-e.startedAt:null;return{...e,status:r,result:t.result,durationMs:n}};if(Zc(t.run_id||M.value||null)&&n&&!t.source_agent){let e=!1;if(N(t=>{let r=[...t];for(let t=0;t<r.length;t++){let i=r[t];if(i.type!==`dm_reasoning`)continue;let o=i.tools.findIndex(e=>e.id===n);if(o>=0){let n=[...i.tools];n[o]=a(n[o]),r[t]={...i,tools:n},e=!0;break}}return r}),e){if(!t.source_agent){let{phase:e}=Cc.value;(e===`tool_active`||e===`executing_tools`)&&Oc(`calling_llm`)}return}}let o=n&&xe(e=>e.type===`tool`&&e.id===n,a);if(o||=xe(e=>e.type===`tool`&&e.status===`running`,a),o){let e=F.value,i=n?e.find(e=>e.type===`tool`&&e.id===n):e.findLast(e=>e.type===`tool`&&e.status===r);if(i&&i.tool===`invoke_agent`){let e=typeof t.result==`object`?t.result:null;if(!(e&&e.task_id)){let t=i.params?.name||i.params?.subagent_name||pc(n);t&&(e&&e.session_id&&gc(t,e.session_id),fc(t,r))}}}else t.source_agent||console.warn(`[tool_end] no matching tool message found for`,n,`- tool messages in chat:`,F.value.filter(e=>e.type===`tool`).length);if(!t.source_agent){let{phase:e}=Cc.value;(e===`tool_active`||e===`executing_tools`)&&Oc(`calling_llm`)}})}),p(`approval_required`,e=>{c(()=>{ll(),pl();let t=JSON.parse(e.data);if(!F.value.some(e=>e.type===`approval`&&e.approvalId===t.approval_id)){let e=kc(t);I({id:P(),type:`approval`,approvalId:e.approvalId,tool:e.tool,params:e.params,runId:e.runId,resolved:!1})}})}),p(`subagent_activity`,e=>{let t=JSON.parse(e.data);t.source_agent&&dc(t.source_agent,t.kind,t.tool||null,t.tool_invocation_id||null,t.parent_tool_invocation_id||null)}),p(`subagent_started`,e=>{c(()=>{let t=JSON.parse(e.data),n=t.subagent_session_id||null;if(!n)return;let r=t.subagent_name||pc(t.tool_invocation_id);if(!r){console.warn(`[subagent_started] cannot resolve target entry`,`— subagent_name:`,t.subagent_name,`tool_invocation_id:`,t.tool_invocation_id);return}gc(r,n)})}),p(`subagent_completed`,e=>{c(()=>{let t=JSON.parse(e.data),n=t.subagent_name||`subagent`,r=t.status||`done`,i=t.subagent_session_id||null,a=t.summary||``,o=t.tool_invocation_id||null,s=pc(o)||mc(i),c=s&&q.value[s]||q.value[n]||Object.values(q.value).find(e=>e.displayName===n||n===`subagent`&&e.status===`running`),l=c?c.task:``,u=c&&c.toolsUsed||0,d=c&&c.startedAt?Date.now()-c.startedAt:null;i&&gc(n,i,o),fc(n,r,o,i),I({id:P(),type:`subagent_completed`,name:n,task:l,status:r,toolCount:u,durationMs:d,sessionId:i,summary:a})})}),p(`job_completed`,e=>{let t=JSON.parse(e.data);I({id:P(),type:`job_completed`,jobName:t.job_name||`job`,status:t.status||`success`,summary:t.summary||``,ts:t.ts||null,runId:t.run_id||null,truncated:t.truncated,jobSessionUuid:t.job_session_uuid||null,jobSessionId:t.job_session_id||null})}),p(`dm_message`,e=>{c(()=>{ll(),pl();let t=JSON.parse(e.data);I({id:P(),type:`agent`,role:`assistant`,text:t.message,fromAgent:t.from_agent,fromAgentId:t.from_agent_id,sealed:!0,ts:t.ts||new Date().toISOString()})})}),p(`dm_conversation_ended`,e=>{let t=JSON.parse(e.data),n=t.peer||`unknown`,r=Ac[t.reason]||t.reason||`conversation ended`,i=t.suppress_banner===!0,a=t.context_id||null,o=!1;for(let e=F.value.length-1;e>=0;e--){let t=F.value[e];if(t.type===`agent`||t.type===`user`||t.type===`thinking`||t.type===`dm_message`||t.type===`dm_reasoning`)break;if(t.type===`dm_ended`){o=a&&t.contextId===a||t.peer===n&&t.reason===r;break}}!i&&!o&&I({id:P(),type:`dm_ended`,peer:n,reason:r,contextId:a}),Ec()}),p(`dm_activity_started`,e=>{let t=JSON.parse(e.data);t.peer&&Dc(t.peer)}),p(`dm_activity_status`,e=>{let t=JSON.parse(e.data),n=X.value||t.peer;n&&Tc(`dm`,n)}),p(`dm_activity_ended`,e=>{let t=JSON.parse(e.data),n=X.value||t.peer;n&&Tc(`dm`,n)}),p(`approval_resolved`,e=>{let t=JSON.parse(e.data);Se(e=>!(e.type===`approval`&&e.approvalId===t.approval_id))}),p(`context_debug`,e=>{c(()=>{let t=JSON.parse(e.data);I({id:P(),type:`context_debug`,messages:t.messages,toolNames:t.tool_names,totalTokens:t.total_tokens,systemTokens:t.system_tokens,historyMessageCount:t.history_message_count,agentId:t.agent_id,agentName:t.agent_name})})}),p(`run_warning`,e=>{let t=JSON.parse(e.data);t.source_agent||c(()=>{ll(),pl();let e=t.warning?.code||`UNKNOWN`,n=t.warning?.message||`Warning`;I({id:P(),type:`warning`,code:e,text:n})})});let m=t=>n=>{if(_e(),c(()=>{ll(),pl();let r=n.data?JSON.parse(n.data):{},i=r.run_id||null,a=Zc(i||M.value),o=``;if(a&&i){if(o=qc.value.get(i)||``,o){let e=new Map(qc.value);e.delete(i),qc.value=e}fl(i)}N(e=>{let n=e.filter(e=>e.type===`tool`).length,s=e=>e.type===`approval`&&!e.resolved&&(!e.runId||!i||e.runId===i),c=e.filter(e=>!s(e)).map(e=>{if(e.type===`dm_reasoning`&&e.runId===i&&e.isLive){let n=o||e.thinkingText||``,r=t===`error`?`failed`:t===`cancelled`?`cancelled`:`done`,i=e.tools.map(e=>e.status===`running`&&r!==`done`?{...e,status:`cancelled`}:e);return{...e,status:r,isLive:!1,thinkingText:n,tools:i}}return e});if(t===`error`){let e=r.error?.code||`INTERNAL`,t=Qc(e,typeof r.error==`string`?r.error:r.error?.message||`Run failed`);c=[...c,{id:P(),type:`error`,code:e,text:t}]}t===`cancelled`&&(c=[...c,{id:P(),type:`system`,text:`(run cancelled)`}]),t===`finished`&&!sl&&!a&&(c=[...c,{id:P(),type:`system`,text:`(run completed)`}]);let l=r.prompt_tokens||r.completion_tokens?{prompt_tokens:r.prompt_tokens||0,completion_tokens:r.completion_tokens||0,reasoning_tokens:r.reasoning_tokens,cache_creation_input_tokens:r.cache_creation_input_tokens,cache_read_input_tokens:r.cache_read_input_tokens}:r.usage;l&&(c=[...c,{id:P(),type:`tokens`,usage:l}]);let u=c.filter(e=>e.type===`tool`).length;return u<n&&console.warn(`[handleRunEnd] tool message count decreased:`,n,`->`,u),c}),M.value=null,X.value?Tc(`dm`,X.value):Ec(),te(e)}),w.value.length>0){let t=w.value[0],n=w.value.slice(1);w.value=n;let r=x.value;fe(e,n),Ys(()=>Promise.resolve().then(()=>zf).then(e=>{e.startRun&&e.startRun(t.text,{sessionId:r})}),void 0).catch(e=>{console.error(`[session-stream] Failed to process queued message:`,e)})}};p(`run_finished`,m(`finished`)),p(`run_error`,m(`error`)),p(`run_cancelled`,m(`cancelled`)),u.onerror=()=>{if(u.readyState===EventSource.CLOSED){if(tl++,tl>=nl){console.error(`[session-stream] Max retries reached`),Fc(`session`);return}let t=Math.min(2e3*2**(tl-1),3e4);rl=setTimeout(()=>{rl=null,x.value===e&&ml(e,{lastEventId:cl})},t)}}}function hl(){tl=0;let e=x.value;e?ml(e,{lastEventId:cl}):Ic(`session`)}Lc(hl);function gl(){al!==null&&(cancelAnimationFrame(al),al=null),ll(),rl!==null&&(clearTimeout(rl),rl=null),sl=!1,Jc.clear(),Yc.clear(),Ec(),el!=null&&(Gc(el),el=null),$c&&=($c.close(),null),Ic(`session`)}function _l(){return $c!==null}var vl=null,yl=null,bl=0,xl=10,Sl=null,Cl=null,wl=null;function Tl(e,t){t.session_id&&((typeof t.has_active_run==`boolean`?t.has_active_run:e===`session_activity_started`)?ie(t.session_id,{runId:e===`session_activity_ended`?null:t.run_id||null,finished:!1}):le(t.session_id))}function El(e){let t={};for(let n of e.sessions||[])n.has_active_run&&(t[n.id]={runId:null,finished:!1});re.value=t}function Dl(e,t){let n=t&&t.streamEpoch!=null?String(t.streamEpoch):null;if(kl(),!e)return;wl!==null&&(clearTimeout(wl),wl=null);let r=localStorage.getItem(`alms_auth_token`),i=new URLSearchParams;r&&i.set(`token`,r),t&&t.lastEventId!=null&&i.set(`last_event_id`,String(t.lastEventId)),n&&i.set(`stream_epoch`,n);let a=i.toString(),o=`/events/session-activity${a?`?`+a:``}`,s=new EventSource(o);vl=s,yl=e,bl=0,Sl=t&&t.lastEventId!=null?t.lastEventId:null,Cl=n;let c=!1,l=!1,u=[],d=!1,f=null,p=0,m=(e,t,n)=>{if(l){u.push({type:e,data:t,eventId:n});return}Tl(e,t)},h=async e=>{let t=Number.isSafeInteger(e)?e:null;if(l){d=!0,t!=null&&(f=f==null?t:Math.max(f,t));return}if(s!==vl)return;l=!0,u=[];let n=null;try{n=await _s(null,{includeDms:!0})}catch(e){console.error(`[agent-events] activity reconciliation failed:`,e)}if(s!==vl)return;n&&(El(n),p=0);let r=u;u=[],l=!1;for(let e of r)t!=null&&e.eventId!=null&&e.eventId<=t||Tl(e.type,e.data);let i=d,a=f;if(d=!1,f=null,i&&s===vl){h(a);return}if(!n&&s===vl){p++;let e=Math.min(1e3*2**(p-1),3e4);s._reconciliationRetryTimer=setTimeout(()=>{s._reconciliationRetryTimer=null,s===vl&&h(null)},e)}};s.addEventListener(`open`,()=>{if(s!==vl)return;let e=c||!!(t&&t.reconcileOnOpen);c=!0,Ic(`agent-events`),e&&h(null)});let g=(e,t)=>s.addEventListener(e,n=>{if(s!==vl)return;let r=n.lastEventId;r&&/^\d+$/.test(r)&&(Sl=r);try{let r=JSON.parse(n.data),i=globalThis.__almsContracts,a=i?i.parseSsePayload(e,r):r;t({data:JSON.stringify(a),lastEventId:n.lastEventId})}catch(t){console.error(`[agent-events]`,e,`handler failed:`,t)}});g(`session_activity_started`,e=>{let t=JSON.parse(e.data),n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;m(`session_activity_started`,t,n)}),g(`session_activity_ended`,e=>{let t=JSON.parse(e.data),n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;m(`session_activity_ended`,t,n)}),g(`stream_state`,e=>{let t=JSON.parse(e.data),n=!!(Cl&&t.stream_epoch&&Cl!==t.stream_epoch);if(t.stream_epoch&&(Cl=t.stream_epoch),t.requires_reconciliation||n){let e=Number.isSafeInteger(t.newest)?t.newest:null;h(e)}}),s.onerror=()=>{if(s.readyState===EventSource.CLOSED){if(bl++,bl>=xl){console.error(`[agent-events] Max retries reached for agent`,e),Fc(`agent-events`);return}let t=Math.min(2e3*2**(bl-1),3e4),n=e,r=Sl,i=Cl;wl=setTimeout(()=>{wl=null,yl===n&&Dl(n,{lastEventId:r,streamEpoch:i,reconcileOnOpen:!0})},t)}}}function Ol(){bl=0;let e=yl;e?Dl(e,{lastEventId:Sl,streamEpoch:Cl,reconcileOnOpen:!0}):Ic(`agent-events`)}Rc(Ol);function kl(){vl&&=(vl._reconciliationRetryTimer!=null&&(clearTimeout(vl._reconciliationRetryTimer),vl._reconciliationRetryTimer=null),vl.close(),null),yl=null,Sl=null,Cl=null,bl=0,wl!==null&&(clearTimeout(wl),wl=null),Ic(`agent-events`)}var Al=e=>m(`/runs`,e),jl=e=>v(`/runs/${e}`),Ml=(e,t=20)=>v(`/runs?session_id=${e}&limit=${t}`),Nl=e=>m(`/runs/${e}/cancel`),Pl=e=>v(`/runs/${e}/reasoning`),Fl=e=>v(`/runs/${e}/text`),Il=e=>v(`/approvals?session_id=${e}`),Ll=(e,t=50)=>v(`/runs?agent_id=${e}&limit=${t}`);function Rl(e,t){let n=e||``,r=t&&t.job_status,i=n.indexOf(`
`),a=i>=0?n.slice(0,i):n,o=i>=0?n.slice(i+1).trim():``,s=a.match(/^\[Scheduled job (\w+)\]\s*(.*)$/);if(!s)return{jobName:n,status:r||`success`,summary:``};let c=s[1];return{jobName:(s[2]||``).trim(),status:r||(c===`failed`?`error`:c===`completed`?`success`:c===`finished`?`cancelled`:`success`),summary:o}}function zl(e){let t=new Map;for(let n of e){let e=n.tool_id;if(!e)continue;let r=t.get(e)||{call:null,result:null,runId:null};n.role===`assistant`||n.role===`Assistant`?(r.call=n,n.run_id&&(r.runId=n.run_id)):(n.role===`tool`||n.role===`Tool`)&&(r.result=n,n.run_id&&!r.runId&&(r.runId=n.run_id)),t.set(e,r)}return t}function Bl(e,t){let n=t&&t.hasActiveRun,r=t&&t.isDm,i=t&&t.sessionToolCalls||[],a=i.length>0?zl(i):new Map,o=new Map,s=new Map;for(let t of e)t.type===`tool_result`&&t.tool_id&&o.set(t.tool_id,t),t.type===`tool_result`&&t.metadata&&t.metadata.tool_invocation_id&&s.set(t.metadata.tool_invocation_id,t);let c=[],l=[],u=(e,t)=>{c.push(e),l.push(t||null)};for(let t of e)if(t.type===`text`||!t.type){if(t.metadata&&t.metadata.message_type===`dm_ended`){let e=t.metadata.reason||``;u({id:P(),type:`dm_ended`,peer:t.metadata.ended_by||`unknown`,reason:Ac[e]||e||`conversation ended`},t.timestamp);continue}if(t.metadata&&t.metadata.message_type===`reasoning`){let e=``;Array.isArray(t.metadata.reasoning_blocks)&&(e=t.metadata.reasoning_blocks.map(e=>e&&typeof e.text==`string`?e.text:``).join(``)),e||=t.content||``,u({id:P(),type:`dm_reasoning_text`,text:e,fromAgent:t.metadata.from_agent||null,runId:t.metadata.run_id||null},t.timestamp);continue}let e=t.role===`system`&&t.metadata&&t.metadata.synthetic,n=t.role===`user`&&t.metadata&&t.metadata.message_type===`dm`&&t.metadata.from_agent;if(e&&t.metadata.type===`job_notification`){let e=Rl(t.content||``,t.metadata);u({id:P(),type:`job_completed`,jobName:e.jobName,status:e.status,summary:e.summary,ts:t.timestamp||null,metadata:t.metadata||null,runId:t.metadata&&t.metadata.run_id||null,truncated:t.metadata?t.metadata.truncated:void 0,jobSessionUuid:t.metadata&&t.metadata.job_session_uuid||null,jobSessionId:t.metadata&&t.metadata.job_session_id||null},t.timestamp);continue}if(e&&t.metadata.type===`run_boundary`){let e=t.metadata.status||`completed`;u({id:P(),type:`run_boundary`,status:e,runId:t.metadata.run_id||null,error:t.metadata.error||null,text:t.content||``},t.timestamp);continue}if(e&&t.metadata.kind===`error`){u({id:P(),type:`error`,text:t.metadata.error?`${t.content}\n\n${t.metadata.error}`.trim():t.content||`Run error`,code:t.metadata.error_kind||t.metadata.type||null},t.timestamp);continue}if(e&&t.metadata.type===`run_warning`){if(t.metadata.source_agent)continue;u({id:P(),type:`warning`,code:t.metadata.code||`UNKNOWN`,text:t.content||`Warning`},t.timestamp);continue}if(e&&t.metadata.type===`subagent_completion`){let e=t.metadata;u({id:P(),type:`subagent_completed`,name:e.subagent_name||`subagent`,task:e.task_description||``,status:e.status||`done`,toolCount:e.tool_count||0,durationMs:e.duration_ms==null?null:e.duration_ms,sessionId:e.session_id||null,summary:e.summary||``,toolInvocationId:e.tool_invocation_id||null},t.timestamp);continue}if(e&&t.metadata.type===`subagent_started`){let e=t.metadata;u({id:P(),type:`subagent_started`,name:e.subagent_name||`subagent`,toolInvocationId:e.tool_invocation_id||null,subagentSessionId:e.subagent_session_id||null},t.timestamp);continue}if(e&&t.metadata.type===`dm_ended_notification`){u({id:P(),type:`notification`,role:`system`,text:t.content||``,metadata:t.metadata,sealed:!0},t.timestamp);continue}let r=e?`notification`:n?`agent`:t.role===`user`?`user`:`agent`,i;r===`agent`&&t.metadata&&Array.isArray(t.metadata.reasoning_blocks)&&(i=t.metadata.reasoning_blocks.map(e=>e&&typeof e.text==`string`?e.text:``).join(``),i||=void 0),u({id:P(),type:r,role:t.role,text:t.content||``,metadata:t.metadata||null,sealed:!0,reasoning:i,fromAgent:n?t.metadata.from_agent:void 0,ts:t.timestamp||null},t.timestamp)}else if(t.type===`tool_call`){let e=t.metadata&&t.metadata.tool_call_id||null,r=t.metadata&&t.metadata.tool_invocation_id||null,i=(e?o.get(e):null)||(r?s.get(r):null),c=t.metadata&&t.metadata.run_id||null,l=t.metadata&&t.metadata.message_type===`reasoning`,d=t.tool,f=t.params,p=i?i.result:null,m=i?i.ok:null,h=c;if(e&&a.has(e)){let t=a.get(e);if(t.call&&(d||=t.call.tool_name||d,!f&&t.call.params))try{f=JSON.parse(t.call.params)}catch{f=null}if(t.result&&p==null){try{p=JSON.parse(t.result.result)}catch{p=t.result.result}m??=!0}!h&&t.runId&&(h=t.runId)}let g=l&&t.metadata&&t.metadata.from_agent?t.metadata.from_agent:void 0;if(!g&&e&&a.has(e)){let t=a.get(e);g=t.call&&t.call.from_agent||t.result&&t.result.from_agent||void 0}u({id:r||e||P(),type:`tool`,tool:d,params:f,status:m==null?n?`running`:`done`:m?`done`:`fail`,result:p,runId:h||void 0,isReasoning:l||void 0,fromAgent:g,ts:t.timestamp||null},t.timestamp)}else if(t.type===`image`){let e=t.role===`user`&&t.metadata&&t.metadata.message_type===`dm`&&t.metadata.from_agent;u({id:P(),type:`image`,role:e?`assistant`:t.role,url:t.url||``,alt:t.alt||``,sealed:!0,fromAgent:e?t.metadata.from_agent:void 0,ts:t.timestamp||null},t.timestamp)}if(i.length>0){let t=new Set;for(let e of c)e.type===`tool`&&e.id&&t.add(e.id);for(let n of e)if(n.type===`tool_call`){let e=n.metadata&&n.metadata.tool_call_id;e&&t.add(e);let r=n.metadata&&n.metadata.tool_invocation_id;r&&t.add(r)}let i=[];for(let[e,o]of a){if(t.has(e)||!o.call)continue;let a=null;if(o.call.params)try{a=JSON.parse(o.call.params)}catch{}let s=null,c=null;if(o.result&&o.result.result){try{s=JSON.parse(o.result.result)}catch{s=o.result.result}c=!0}let l=o.call.from_agent||o.result&&o.result.from_agent||void 0;i.push({entry:{id:e||P(),type:`tool`,tool:o.call.tool_name||`unknown`,params:a,status:c==null?n?`running`:`done`:c?`done`:`fail`,result:s,runId:o.runId||void 0,isReasoning:r||void 0,fromAgent:l,ts:o.call.timestamp||null},ts:o.call.timestamp||null})}if(i.length>0){i.sort((e,t)=>!e.ts&&!t.ts?0:e.ts?t.ts?e.ts<t.ts?-1:+(e.ts>t.ts):-1:1);let e=0;for(let{entry:t,ts:n}of i){if(!n){c.push(t),l.push(null);continue}let r=c.length;for(let t=e;t<l.length;t++)if(l[t]&&l[t]>n){r=t;break}c.splice(r,0,t),l.splice(r,0,n),e=r+1}}}return c}function Vl(e){let t=new Map,n=new Set,r=null;for(let i=0;i<e.length;i++){let a=e[i];if(a.type===`dm_reasoning_text`&&a.runId){n.add(i);let e=t.get(a.runId)||{agentName:null,thinkingText:``,tools:[],firstIdx:i};e.agentName=e.agentName||a.fromAgent,e.thinkingText=(e.thinkingText||``)+(a.text||``),t.has(a.runId)||t.set(a.runId,e),r=a.runId;continue}if(a.type===`tool`&&a.runId){n.add(i);let e=t.get(a.runId)||{agentName:null,thinkingText:``,tools:[],firstIdx:i};e.tools.push(a),e.agentName=e.agentName||a.fromAgent,t.has(a.runId)||t.set(a.runId,e),r=a.runId;continue}if(a.type===`tool`&&!a.runId&&r&&t.has(r)){n.add(i);let e=t.get(r);e.tools.push(a),e.agentName=e.agentName||a.fromAgent;continue}a.type!==`tool`&&(r=null)}if(t.size===0)return e;let i=[],a=new Set;for(let r=0;r<e.length;r++){if(n.has(r)){let n=e[r].runId;if(n&&t.has(n)&&!a.has(n)){let e=t.get(n);(e.tools.length>0||e.thinkingText&&e.thinkingText.trim())&&i.push({id:P(),type:`dm_reasoning`,runId:n,agentName:e.agentName,thinkingText:e.thinkingText||``,tools:e.tools,status:`done`,isLive:!1}),a.add(n)}continue}i.push(e[r])}return i}function Hl(e,t){return typeof t==`number`&&Number.isFinite(t)&&e!=null&&Number.isFinite(Number(e))&&Number(e)>=t}var Ul=e({loadSession:()=>Jl}),Wl=new Set([`subagent`,`job`,`episodic`,`notification`]),Gl=200,Kl=100;function ql(e,t){return t||_.value.find(t=>t.id===e)||D.value.find(t=>t.id===e)||null}async function Jl(e,t){let n=t.isStale,r=t.logPrefix||`loadSession`,i=null;try{let t=await bs(e);if(n())return;i=t||null,t&&Wl.has(t.session_type)&&!_.value.some(e=>e.id===t.id)&&(_.value=[..._.value,t]),t&&t.session_type===`dm`&&!D.value.some(e=>e.id===t.id)&&(D.value=[...D.value,t]),t&&Object.prototype.hasOwnProperty.call(t,`parent_session_id`)?tc.value=t.parent_session_id??null:tc.value=null}catch(e){if(n())return;console.warn(`[${r}] Failed to fetch session metadata:`,e)}try{let t=await Ml(e,Gl);if(n())return;let r=t.runs||[];j.value=r;let i=r.find(e=>e.status===`running`)||r.find(e=>e.status===`queued`);i&&(M.value=i.run_id)}catch{if(n())return;j.value=[]}let a=null,o=!1,s=new Set;try{let[t,s]=await Promise.all([ys(e),Ss(e).catch(e=>(console.warn(`[${r}] Failed to load session tool calls:`,e),{tool_calls:[]}))]);if(n())return;let c=t.messages||[],l=s.tool_calls||[];o=ql(e,i)?.session_type===`dm`;let u=Bl(c,{hasActiveRun:!!M.value,sessionToolCalls:l,isDm:o}),d=c.filter(e=>e.type===`tool_call`).length,f=u.filter(e=>e.type===`tool`).length;(d>0||f>0||l.length>0)&&console.debug(`[${r}] history loaded:`,c.length,`API messages,`,d,`tool_calls ->`,f,`tool rows,`,l.length,`session tool call records`);let p=ce(e);if(p){let t=!1;if(p.runId)t=!!j.value.find(e=>e.run_id===p.runId);else{let e=u.findLast(e=>e.type===`user`);t=e&&e.text===p.text}t?te(e):(u.push({id:P(),type:`user`,role:`user`,text:p.text,sealed:!0,ts:p.ts||new Date().toISOString()}),console.debug(`[${r}] re-injected pending user message for session`,e))}be(o?Vl(u):u),Sc(u),a=t.last_event_id??null}catch(e){if(n())return;be([{id:P(),type:`error`,text:`Failed to load message history: ${e.error?.message||e.message||`unknown error`}`}])}if(M.value){if(!F.value.some(e=>e.type===`thinking`)){let e=j.value.find(e=>e.run_id===M.value),t=e&&e.status===`queued`,i=+!!t;if(t)try{let e=await jl(M.value);if(n())return;typeof e?.queue_position==`number`&&e.queue_position>0&&(i=e.queue_position)}catch(e){console.warn(`[${r}] Failed to load queue position:`,e)}I({id:P(),type:`thinking`,queuedBehind:i,runId:M.value})}try{let t=await Il(e);if(n())return;let r=t.approvals||[];r.length>0&&I(...r.map(e=>{let t=kc(e);return{id:P(),type:`approval`,approvalId:t.approvalId,tool:t.tool,params:t.params,runId:t.runId,resolved:!1}}))}catch(e){console.warn(`[${r}] Failed to load pending approvals:`,e)}{let e=M.value,t=j.value.find(t=>t.run_id===e),i=t&&(t.status===`running`||t.status===`queued`);try{let t=await Pl(e);if(n())return;t?.terminal===!0&&(Hl(a,t.seal_event_id)&&s.add(e),M.value=null,Se(t=>!(t.type===`thinking`&&t.runId===e))),o||(i&&t?.text&&I({id:P(),type:`agent`,role:`assistant`,text:``,reasoning:t.text,sealed:!1,ts:new Date().toISOString()}),t?.last_event_id!=null&&(a==null||t.last_event_id>a)&&(a=t.last_event_id))}catch(e){console.warn(`[${r}] Failed to load in-flight reasoning:`,e)}if(!o)try{let t=await Fl(e);if(n())return;i&&t?.text&&(xe(e=>e.type===`agent`&&!e.sealed,e=>({...e,text:(e.text||``)+t.text}))||I({id:P(),type:`agent`,role:`assistant`,text:t.text,reasoning:``,sealed:!1,ts:new Date().toISOString()})),t?.last_event_id!=null&&(a==null||t.last_event_id>a)&&(a=t.last_event_id)}catch(e){console.warn(`[${r}] Failed to load in-flight text:`,e)}}}if(!n())if(ml(e,{lastEventId:a,sealedReasoningRunIds:s}),M.value){let t=j.value.find(e=>e.run_id===M.value);if(!(t&&t.status===`queued`)){let t=ql(e,i);if(t&&t.session_type===`dm`&&Array.isArray(t.participants)){let e=A.value?.name,n=e?t.participants.find(t=>t!==e):t.participants[0];n?Dc(n):Tc(`calling_llm`,null)}else Tc(`calling_llm`,null)}}else{let t=A.value?.id;t?Yl(t,e,n,r).catch(e=>console.warn(`[${r}] restoreGlobalAgentPhase uncaught:`,e)):Ec()}}async function Yl(e,t,n,r){try{let t=await Ll(e,Kl);if(n())return;let i=t.runs||[],a=i.find(e=>e.session_type===`dm`&&e.status===`running`);if(a&&a.context_id){let e=A.value?.name,t=a.context_id.split(`:`);if(t.length>=3&&t[0]===`dm`&&e){let n=t[1]===e?t[2]:t[1];if(n){Dc(n),console.debug(`[${r}] restored cross-session DM status: Chatting with ${n}`);return}}}i.find(e=>e.status===`running`)?Tc(`calling_llm`,null):Ec()}catch(e){console.warn(`[${r}] Failed to check agent global status:`,e),Ec()}}function Xl(e,t){return!t||typeof t!=`string`?e??null:t===e?null:t}function Zl(e){return!e||typeof e!=`string`?null:e}function Ql(e,t){return!t||typeof t!=`string`||!e||typeof e!=`string`?!1:e===t}function $l(e){let t=new Map;if(!Array.isArray(e))return t;for(let n of e){if(!n||typeof n.agent_id!=`string`||!n.agent_id)continue;let e=t.get(n.agent_id);e?e.push(n):t.set(n.agent_id,[n])}return t}function eu(e,t){if(!e||!t||typeof t!=`string`)return!1;if(e.session_type===`notification`)return e.agent_name===t;if(e.session_type===`dm`){let n=e.participants;return Array.isArray(n)&&n.includes(t)}return!1}function tu(e,t){if(!Array.isArray(e))return[];let n=e.map((e,n)=>({s:e,idx:n,owned:+!eu(e,t)}));return n.sort((e,t)=>e.owned-t.owned||e.idx-t.idx),n.map(e=>e.s)}function nu(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type!==`notification`&&e.session_type!==`job`):[]}function ru(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type===`job`):[]}var iu=e({boot:()=>du,fetchCrossAgentSurfaces:()=>fu,saveActiveSession:()=>cu,switchAgent:()=>mu}),au=`alms_active_agent`,ou=0;function su(e){return`alms_active_session_${e}`}function cu(e,t){e&&t&&localStorage.setItem(su(e),t)}function lu(e,t,n){if(n){let e=t.find(e=>e.id===n);if(e)return e}let r=localStorage.getItem(su(e));if(r){let e=t.find(e=>e.id===r);if(e)return e}return t[0]||null}async function uu(e,t){let n=localStorage.getItem(su(e));if(!n||t.some(e=>e.id===n))return null;try{return await ys(n),n}catch(t){return t&&t.status===404&&localStorage.removeItem(su(e)),null}}async function du(){try{let e=await hs();Ts.value=e,k.value=e.agents||[];let t=localStorage.getItem(au),n=k.value.find(e=>e.is_default),r=k.value[0],i=k.value.find(e=>e.id===t)||n||r;i&&(O.value=i.id,ge.value=Zl(i.id),localStorage.setItem(au,i.id),await pu(i.id))}catch(e){throw console.error(`[boot] failed:`,e),e}}async function fu(){try{return(await _s(null,{includeDms:!0})).sessions||[]}catch(e){return console.error(`[fetchCrossAgentSurfaces] failed:`,e),[]}}async function pu(e,t){let n=++ou;try{let[r,i]=await Promise.all([_s(e,{includeDms:!1}),fu()]);if(n!==ou)return;let a=nu(r.sessions||[]);_.value=a,D.value=i;let o={};for(let e of[...a,...i])e.has_active_run&&(o[e.id]={runId:null,finished:!1});re.value=o,Dl(e);let s=t?null:await uu(e,a);if(n!==ou)return;if(s)x.value=s,cu(e,s),await Jl(s,{isStale:()=>n!==ou,logPrefix:`loadAgentSessions:hidden`});else if(a.length>0){let r=lu(e,a,t);x.value=r.id,cu(e,r.id),await Jl(r.id,{isStale:()=>n!==ou,logPrefix:`loadAgentSessions`})}else{let t=await vs(e,`web-chat-`+Date.now());if(n!==ou)return;let[r,i]=await Promise.all([_s(e,{includeDms:!1}),fu()]);if(n!==ou)return;_.value=nu(r.sessions||[]),D.value=i,x.value=t.session_id,be([]),j.value=[],ml(t.session_id)}}catch(e){if(n!==ou)return;console.error(`[loadAgentSessions] failed:`,e)}}async function mu(e,t){if(!k.value.find(t=>t.id===e))return;gl(),kl(),we(),O.value=e,ge.value=Zl(e),localStorage.setItem(au,e),js.value=!0,x.value=null,M.value=null,ve.value=null,_.value=[],D.value=[],j.value=[],be([]),w.value=[],Ds.value=null,Os.value=null,re.value={},xc();let n=pu(e,t&&t.targetSessionId),r=ou;try{await n}finally{r===ou&&(js.value=!1)}}var hu=d(null),Z=d(`agents`);function gu(e){hu.value===e?hu.value=null:(hu.value=e,Z.value=e)}var _u=`alms_theme`;function vu(){return localStorage.getItem(_u)||`dark`}var yu=d(vu());function bu(){let e=yu.value===`dark`?`light`:`dark`;yu.value=e,localStorage.setItem(_u,e),document.documentElement.setAttribute(`data-theme`,e)}document.documentElement.setAttribute(`data-theme`,vu());var xu=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="8"/><path d="M10 6v4l3 3"/></svg>`,Su=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 5a2 2 0 012-2h3l2 2h5a2 2 0 012 2v7a2 2 0 01-2 2H5a2 2 0 01-2-2V5z"/></svg>`,Cu=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M5 5l10 10M15 5L5 15"/></svg>`,wu=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 15V5M10 5L5 10M10 5l5 5"/></svg>`,Tu=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor"><rect x="5" y="5" width="10" height="10" rx="1.5"/></svg>`,Eu=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l10 6-10 6V4z"/></svg>`,Du=()=>f`<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 00-2 2v.18a2 2 0 01-1 1.73l-.43.25a2 2 0 01-2 0l-.15-.08a2 2 0 00-2.73.73l-.22.38a2 2 0 00.73 2.73l.15.1a2 2 0 011 1.72v.51a2 2 0 01-1 1.74l-.15.09a2 2 0 00-.73 2.73l.22.38a2 2 0 002.73.73l.15-.08a2 2 0 012 0l.43.25a2 2 0 011 1.73V20a2 2 0 002 2h.44a2 2 0 002-2v-.18a2 2 0 011-1.73l.43-.25a2 2 0 012 0l.15.08a2 2 0 002.73-.73l.22-.39a2 2 0 00-.73-2.73l-.15-.08a2 2 0 01-1-1.74v-.5a2 2 0 011-1.74l.15-.09a2 2 0 00.73-2.73l-.22-.38a2 2 0 00-2.73-.73l-.15.08a2 2 0 01-2 0l-.43-.25a2 2 0 01-1-1.73V4a2 2 0 00-2-2z"/><circle cx="12" cy="12" r="3"/></svg>`,Ou=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M3 5h14M3 10h14M3 15h14"/></svg>`,ku=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="4"/><path d="M10 2v2M10 16v2M3.5 10H2M18 10h-1.5M5.05 5.05L3.63 3.63M16.37 16.37l-1.42-1.42M5.05 14.95l-1.42 1.42M16.37 3.63l-1.42 1.42"/></svg>`,Au=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 12.5A7.5 7.5 0 017.5 3 7.5 7.5 0 1017 12.5z"/></svg>`,ju=d(!1);function Mu(){ju.value=!ju.value}function Nu(){ju.value=!1}var Pu=[`agents`,`jobs`,`audit`],Fu=a(()=>Ts.value.posture||`guarded`);function Iu({onOpenSettings:e,status:t}){let n=Fu.value,r=t.value===`connected`?`ok`:t.value===`running`?`running`:t.value===`error`||t.value===`offline`?`error`:``;return f`
        <header>
            <button class="sidebar-toggle-btn" title="Toggle sessions" aria-label="Toggle sessions"
                    onClick=${Mu}>
                ${ju.value?f`<${Cu} />`:f`<${Ou} />`}
            </button>
            <h1>ALMS</h1>

            ${n===`guarded`&&f`
                <span id="posture-badge" class="guarded">guarded</span>
            `}
            ${n===`autonomous`&&f`
                <span id="posture-badge" class="autonomous">autonomous</span>
            `}

            <div class="header-spacer"></div>

            <span class="status-dot ${r}" aria-hidden="true"></span>
            <span id="status">${t.value}</span>
            ${Ms.value&&f`
                <button class="retry-btn" onClick=${Fs}>Retry</button>
            `}

            <div class="header-btns">
                ${Pu.map(e=>f`
                    <button class="hbtn ${hu.value===e?`active`:``}"
                            onClick=${()=>gu(e)}>
                        ${e.charAt(0).toUpperCase()+e.slice(1)}
                    </button>
                `)}
            </div>

            <button class="header-icon-btn" title="Toggle theme" aria-label="Toggle theme"
                    onClick=${bu}>
                ${yu.value===`dark`?f`<${ku} />`:f`<${Au} />`}
            </button>

            <button class="header-icon-btn settings-btn" title="Settings" aria-label="Settings"
                    onClick=${e}>
                <${Du} />
            </button>
        </header>
    `}async function Lu(e,t){if(!e||e===x.value)return;Nu();let n=_.value.find(t=>t.id===e)||D.value.find(t=>t.id===e);if(n&&n.session_type!==`dm`&&n.session_type!==`notification`&&n.session_type!==`job`&&n.agent_id&&O.value&&n.agent_id!==O.value&&k.value.some(e=>e.id===n.agent_id)){await mu(n.agent_id,{targetSessionId:e});return}let r=we();gl(),c(()=>{x.value=e,M.value=null,ve.value=null,be([]),w.value=[],Os.value=null,xc(),tc.value=null,As.value=!0}),cu(O.value,e);try{await Jl(e,{isStale:()=>r!==Ce,logPrefix:t&&t.logPrefix||`navigateToSession`})}finally{r===Ce&&(As.value=!1)}}var Ru={chat:{icon:`▸`,cls:``,label:`Chat session`},dm:{icon:`↔`,cls:`dm`,label:`DM conversation`},notification:{icon:`⚡`,cls:`notification`,label:`Notification session`},job:{icon:`⏰`,cls:`job`,label:`Job session`},subagent:{icon:`⚙`,cls:`subagent`,label:`Subagent session`},telegram:{icon:`✉`,cls:`telegram`,label:`Telegram session`}};function zu(e){return Ru[e.session_type]||Ru.chat}function Bu(e,t,n,r){if(e===t&&n)return!0;let i=r[e];return!!(i&&!i.finished)}function Vu(e){return Lu(e,{logPrefix:`selectSession`})}async function Hu(){if(O.value){gl(),we();try{let e=`web-chat-`+Date.now(),t=await vs(O.value,e),[n,r]=await Promise.all([_s(O.value,{includeDms:!0}),fu()]);c(()=>{_.value=n.sessions||[],D.value=r,x.value=t.session_id,cu(O.value,t.session_id),M.value=null,ve.value=null,be([]),w.value=[],j.value=[],Os.value=null,xc()}),ml(t.session_id)}catch(e){console.error(`[newSession] failed:`,e)}}}function Uu(e){let t=e.participants;return Array.isArray(t)&&t.length>=2?t.join(` <-> `):e.context_id||e.id.slice(0,8)}function Wu(e){return e.agent_name?`notifications`:e.context_id||e.id.slice(0,8)}function Gu(e){let t=e.context_id||``;return t.startsWith(`job_`)&&t.length>4?`job `+t.slice(4,12):t||e.id.slice(0,8)}function Ku(e){return e.session_type===`dm`?Uu(e):e.session_type===`notification`?Wu(e):e.session_type===`job`?Gu(e):e.context_id||e.id.slice(0,8)}function qu(e){if(e.session_type===`notification`&&e.agent_name)return e.agent_name;if(e.session_type===`job`&&e.agent_id){let t=k.value.find(t=>t.id===e.agent_id);return t?t.name:null}return null}function Ju({session:e,activeAgentName:t}){let n=u(!1),r=u(null),i=x.value,a=e.id===i,o=Bu(e.id,i,M.value,re.value),s=zu(e),l=s.cls?` session-item-`+s.cls:``,d=e=>{e.stopPropagation(),n.value=!0,r.value=setTimeout(()=>{n.value=!1},3e3)},p=async t=>{t.stopPropagation(),r.value&&=(clearTimeout(r.value),null),n.value=!1;try{await xs(e.id),pe(e.id),e.id===x.value&&(gl(),c(()=>{x.value=null,M.value=null,ve.value=null,be([]),j.value=[],Os.value=null,xc(),w.value=[]}));let[t,n]=await Promise.all([_s(O.value,{includeDms:!0}),fu()]);_.value=t.sessions||[],D.value=n}catch(e){console.error(`[deleteSession] failed:`,e)}},m=e=>{e.stopPropagation(),r.value&&=(clearTimeout(r.value),null),n.value=!1},h=Ku(e),g=e.session_type===`chat`?``:`
Type: `+e.session_type,v=qu(e);return f`
        <div class="session-item${l}${eu(e,t)?` session-item-active-agent`:``} ${a?`active`:``} ${o?`has-run`:``}"
             role="option"
             aria-selected=${a}
             tabindex="0"
             title=${`ID: `+e.id+`
Context: `+e.context_id+g}
             onClick=${()=>Vu(e.id)}
             onKeyDown=${t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),Vu(e.id))}}>
            ${e.session_type!==`chat`&&f`<span class="session-type-icon session-type-icon-${s.cls||`default`}" aria-hidden="true" title=${s.label}>${s.icon}</span>`}
            <span class="session-label">${h}</span>
            ${v&&f`<span class="session-agent-attribution" title=${`Owned by `+v}>${v}</span>`}
            ${n.value?f`
                    <span class="session-delete-confirm-group" role="group" aria-label="Confirm delete">
                        <button class="session-confirm-btn session-confirm-yes"
                                title="Confirm delete (destructive)"
                                aria-label="Confirm delete"
                                onClick=${p}
                                onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),p(e))}}>Yes</button>
                        <button class="session-confirm-btn session-confirm-no"
                                title="Cancel delete"
                                aria-label="Cancel delete"
                                onClick=${m}
                                onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),m(e))}}>No</button>
                    </span>
                `:f`
                    <button class="session-delete-btn"
                            title="Delete session"
                            aria-label="Delete session"
                            onClick=${d}
                            onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),d(e))}}>\u00D7</button>
                `}
        </div>
    `}function Yu({label:e,cls:t,id:n}){return f`
        <div class="session-section-divider ${t||``}" role="presentation" id=${n}>
            <span class="session-section-divider-label">${e}</span>
        </div>
    `}function Xu({expanded:e,count:t,headerId:n}){let r=e=>{e.stopPropagation(),b.value=!b.value};return f`
        <div class="session-section-divider session-divider-job session-section-toggle ${e?`expanded`:``}"
             id=${n}
             role="button"
             tabindex="0"
             aria-expanded=${e}
             title=${e?`Collapse jobs`:`Expand jobs`}
             onClick=${r}
             onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),r(e))}}>
            <span class="agent-group-chevron" aria-hidden="true">${`▸`}</span>
            <span class="session-section-divider-label">
                <span class="session-type-icon session-type-icon-job" aria-hidden="true">${`⏰`}</span>
                Jobs
            </span>
            <span class="agent-group-count" title=${t+` job session`+(t===1?``:`s`)}>${t}</span>
        </div>
    `}function Zu(e){let t=Xl(ge.value,e);ge.value=t,t&&t!==O.value&&mu(t)}function Qu({agent:e,expanded:t,sessionCount:n,isActive:r,headerId:i}){let a=t=>{t.stopPropagation(),Zu(e.id)},o=t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),Zu(e.id))},s=r?t?`Collapse sessions`:`Expand sessions`:`Switch to `+e.name;return f`
        <div class="agent-group-header ${t?`expanded`:``} ${r?`active`:``}"
             id=${i}
             role="button"
             tabindex="0"
             aria-expanded=${t}
             title=${s}
             onClick=${a}
             onKeyDown=${o}>
            <span class="agent-group-chevron" aria-hidden="true">${`▸`}</span>
            <span class="agent-group-name">${e.name}</span>
            ${n!=null&&f`
                <span class="agent-group-count" title=${n+` session`+(n===1?``:`s`)}>
                    ${n}
                </span>
            `}
        </div>
    `}function $u(e){let t=new Set,n=[];for(let r of e){if(r.session_type!==`dm`||!Array.isArray(r.participants)||r.participants.length<2)continue;let e=r.context_id||r.id;t.has(e)||(t.add(e),n.push(r))}return n}function ed(){let e=_.value,t=D.value,n=k.value,r=O.value,i=ge.value,a=A.value?A.value.name:null,o=$l(e.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`&&e.session_type!==`subagent`&&e.session_type!==`episodic`)),s=$l(t.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`)),c=tu($u(t),a),l=tu(t.filter(e=>e.session_type===`notification`),a),u=ru(t),d=b.value;return f`
        <div class="sidebar-section" style="flex:1; min-height:0">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${(!n||n.length===0)&&c.length===0&&l.length===0&&u.length===0?f`<div class="empty-state">No sessions</div>`:null}
                ${n.map(e=>{let t=Ql(i,e.id),n=e.id===r,c=o.get(e.id)||[],l=n?c.length:(s.get(e.id)||[]).length,u=`agent-group-header-`+e.id;return f`
                        <div class="agent-group" key=${e.id}>
                            <${Qu}
                                agent=${e}
                                expanded=${t}
                                sessionCount=${l}
                                isActive=${n}
                                headerId=${u} />
                            <div class="agent-group-body"
                                 role="group"
                                 aria-labelledby=${u}
                                 data-expanded=${t}>
                                <div class="agent-group-sessions">
                                    ${c.length===0?f`<div class="empty-state agent-group-empty">No sessions</div>`:c.map(e=>f`
                                            <${Ju} key=${e.id} session=${e} activeAgentName=${a} />
                                        `)}
                                </div>
                            </div>
                        </div>
                    `})}
                ${c.length>0&&f`
                    <${Yu} label="Direct messages"
                                       cls="session-divider-dm"
                                       id="session-section-dms" />
                    <div role="group" aria-labelledby="session-section-dms">
                        ${c.map(e=>f`
                            <${Ju} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${l.length>0&&f`
                    <${Yu} label="Notifications"
                                       cls="session-divider-notification"
                                       id="session-section-notifications" />
                    <div role="group" aria-labelledby="session-section-notifications">
                        ${l.map(e=>f`
                            <${Ju} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${u.length>0&&f`
                    <${Xu} expanded=${d}
                                          count=${u.length}
                                          headerId="session-section-jobs" />
                    <div class="agent-group-body"
                         role="group"
                         aria-labelledby="session-section-jobs"
                         data-expanded=${d}>
                        <div class="agent-group-sessions">
                            ${u.map(e=>f`
                                <${Ju} key=${e.id} session=${e} activeAgentName=${a} />
                            `)}
                        </div>
                    </div>
                `}
            </div>
            <button id="new-session-btn" onClick=${Hu}>+ New session</button>
        </div>
    `}function td(){let e=ju.value?` sidebar-open`:``;return f`
        ${ju.value&&f`<div class="sidebar-backdrop" onClick=${Nu}></div>`}
        <div id="sidebar" class=${e}>
            <${ed} />
        </div>
    `}function nd(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function rd(e){if(!e)return``;let t=new Date(e),n=new Date;return t.toDateString()===n.toDateString()?nd(e):t.toLocaleDateString([],{month:`short`,day:`numeric`})}function id(e){if(!e)return``;let t=new Date(e);if(isNaN(t.getTime()))return``;let n=new Date,r=t.toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`});return t.toDateString()===n.toDateString()?r:`${t.toLocaleDateString([],{month:`short`,day:`numeric`})} ${r}`}function ad(e){e&&(e.scrollTop=e.scrollHeight)}function od(e){if(!e)return``;let t=``;if(typeof e.querySelector==`function`){let n=e.querySelector(`code`);n&&typeof n.textContent==`string`&&(t=n.textContent)}return!t&&typeof e.textContent==`string`&&(t=e.textContent),t?(t.endsWith(`\r
`)?t=t.slice(0,-2):t.endsWith(`
`)&&(t=t.slice(0,-1)),t):``}function sd(){return!!(typeof navigator<`u`&&navigator.clipboard&&typeof navigator.clipboard.writeText==`function`)}var cd=`cb-copy-decorated`,ld=`code-block-wrapper`,ud=`code-block-copy`,dd=`code-block-copy--copied`,fd=`alms-code-copy-live`;function pd(){if(typeof document>`u`)return null;let e=document.getElementById(fd);return e||(e=document.createElement(`div`),e.id=fd,e.setAttribute(`aria-live`,`polite`),e.setAttribute(`role`,`status`),e.style.position=`absolute`,e.style.width=`1px`,e.style.height=`1px`,e.style.padding=`0`,e.style.margin=`-1px`,e.style.overflow=`hidden`,e.style.clip=`rect(0, 0, 0, 0)`,e.style.whiteSpace=`nowrap`,e.style.border=`0`,document.body.appendChild(e),e)}function md(){let e=pd();e&&(e.textContent=``,setTimeout(()=>{e.textContent=`Copied to clipboard`},50))}function hd(){return[`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" `,`stroke="currentColor" stroke-width="1.5" stroke-linecap="round" `,`stroke-linejoin="round" aria-hidden="true">`,`<rect x="7" y="7" width="10" height="10" rx="1.5"/>`,`<path d="M5 13H4a1 1 0 01-1-1V4a1 1 0 011-1h8a1 1 0 011 1v1"/>`,`</svg>`].join(``)}function gd(){return[`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" `,`stroke="currentColor" stroke-width="2" stroke-linecap="round" `,`stroke-linejoin="round" aria-hidden="true">`,`<path d="M4 10l4 4 8-8"/>`,`</svg>`].join(``)}function _d(e){if(typeof document>`u`)return!1;let t=document.createElement(`textarea`);t.value=e,t.style.position=`fixed`,t.style.top=`-9999px`,t.style.left=`-9999px`,t.setAttribute(`readonly`,``),t.setAttribute(`aria-hidden`,`true`),document.body.appendChild(t);let n=!1;try{t.select(),t.setSelectionRange(0,e.length),n=document.execCommand&&document.execCommand(`copy`)}catch{n=!1}return document.body.removeChild(t),!!n}function vd(e){e&&(e._copyRevertTimer&&=(clearTimeout(e._copyRevertTimer),null),e.classList.add(dd),e.innerHTML=gd(),e.setAttribute(`aria-label`,`Copied`),e.title=`Copied`,md(),e._copyRevertTimer=setTimeout(()=>{e.classList.remove(dd),e.innerHTML=hd(),e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e._copyRevertTimer=null},1500))}function yd(e,t,n){e.preventDefault(),e.stopPropagation();let r=od(t);if(r){if(sd()){navigator.clipboard.writeText(r).then(()=>vd(n),()=>{_d(r)&&vd(n)});return}_d(r)&&vd(n)}}function bd(e,t=`pre`){if(!e||typeof e.querySelectorAll!=`function`)return;let n=e.querySelectorAll(`.${ld}`);for(let e=0;e<n.length;e++){let t=n[e];if(!t.parentNode)continue;let r=t.querySelector(`pre`);if(!r){t.parentNode.removeChild(t);continue}if(!r.classList.contains(cd)){let e=t.parentNode,n=Array.from(t.childNodes);for(let r=0;r<n.length;r++){let i=n[r];i.nodeType===1&&i.classList&&i.classList.contains(ud)||e.insertBefore(i,t)}e.removeChild(t)}}let r=e.querySelectorAll(t);for(let e=0;e<r.length;e++){let t=r[e];if(t.classList.contains(cd))continue;let n=t.parentNode;if(!n)continue;if(n.classList&&n.classList.contains(ld)){if(!n.querySelector(`.${ud}`)){let e=document.createElement(`button`);e.type=`button`,e.className=ud,e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e.innerHTML=hd(),e.addEventListener(`click`,n=>yd(n,t,e)),n.appendChild(e)}t.classList.add(cd);continue}if(!((t.textContent||``).trim().length>0)){t.classList.add(cd);continue}let i=document.createElement(`div`);i.className=ld,n.insertBefore(i,t),i.appendChild(t);let a=document.createElement(`button`);a.type=`button`,a.className=ud,a.setAttribute(`aria-label`,`Copy code`),a.title=`Copy code`,a.innerHTML=hd(),a.addEventListener(`click`,e=>yd(e,t,a)),i.appendChild(a),t.classList.add(cd)}}function xd({ts:e}){if(!e)return null;let t=id(e);return t?f`<span class="msg-timestamp" title=${e}>${t}</span>`:null}function Sd({text:e,live:t}){let n=u(!1);if(!e)return null;let r=()=>{n.value=!n.value},i=e.length>0?` (${e.length} chars)`:``,a=t?`Thinking…`:`Reasoning`,o=n.value?`▼`:`▶`;return f`
        <div class="reasoning-panel ${t?`reasoning-panel--live`:``} ${n.value?`reasoning-panel--open`:``}">
            <button class="reasoning-panel-toggle" onClick=${r}
                    aria-expanded=${n.value}>
                <span class="reasoning-panel-arrow">${o}</span>
                <span class="reasoning-panel-glyph">\u{1F4AD}</span>
                <span class="reasoning-panel-title">${a}</span>
                <span class="reasoning-panel-hint">${i}</span>
            </button>
            ${n.value&&f`
                <div class="reasoning-panel-body">
                    <pre class="reasoning-panel-text">${e}</pre>
                </div>
            `}
        </div>
    `}function Cd({html:e}){let t=n(null);return p(()=>{bd(t.current)},[e]),f`
        <div class="msg-body markdown-body" ref=${t}
             dangerouslySetInnerHTML=${{__html:e}} />
    `}function wd({type:e,role:t,text:n,sealed:r,fromAgent:i,reasoning:a,ts:o}){let c=e===`user`?`user`:`agent`,l=E.value||A.value?.name,u=e===`user`?`>`:i?`${i} $`:l?`${l} $`:`$`,d=e===`agent`&&r===!1,p=e===`agent`&&a?f`<${Sd} text=${a} live=${d} />`:null,m=typeof n==`string`&&n.trim().length>0,h=o&&!d;if(e===`agent`&&r){let e=m?s(n):``;return f`
            <div class="msg ${c}">
                <div class="msg-label-row">
                    <div class="msg-label">${u}</div>
                    ${h&&f`<${xd} ts=${o} />`}
                </div>
                ${p}
                ${m&&f`<${Cd} html=${e} />`}
            </div>
        `}return f`
        <div class="msg ${c}">
            <div class="msg-label-row">
                <div class="msg-label">${u}</div>
                ${h&&f`<${xd} ts=${o} />`}
            </div>
            ${p}
            ${(m||d)&&f`
                <div class="msg-body ${d?`streaming-cursor`:``}">${n}</div>
            `}
        </div>
    `}function Td({usage:e}){if(!e)return null;let t=e.prompt_tokens||0,n=e.completion_tokens||0;if(t+n===0)return null;let r=e.reasoning_tokens;return f`<div class="msg-tokens">${t}p + ${n}c${typeof r==`number`&&r>0?` + ${r}r`:``} tokens</div>`}function Ed({text:e,code:t}){return f`
        <div class="msg msg-error ${t?`msg-error--${t.toLowerCase()}`:``}" data-code=${t||``}>
            <div class="msg-error-icon">\u274C</div>
            <div class="msg-error-body">
                <div class="msg-error-title">Error</div>
                <div class="msg-error-text">${e}</div>
            </div>
        </div>
    `}function Dd({id:e,text:t,code:n}){let r=u(!1),i=u(!1);return i.value?null:f`
        <div class="msg msg-warning ${r.value?`msg-warning--collapsed`:``}" data-code=${n||``}>
            <div class="msg-warning-icon">\u26A0\uFE0F</div>
            <div class="msg-warning-body">
                <div class="msg-warning-header" onClick=${()=>{r.value=!r.value}}>
                    <div class="msg-warning-title">Warning</div>
                    ${n&&f`<span class="msg-warning-code">${n}</span>`}
                    <button class="msg-warning-toggle"
                            title=${r.value?`Expand`:`Collapse`}
                            aria-label=${r.value?`Expand warning`:`Collapse warning`}
                            aria-expanded=${!r.value}>
                        ${r.value?`▶`:`▼`}
                    </button>
                    <button class="msg-warning-dismiss" onClick=${t=>{t.stopPropagation(),i.value=!0,e&&Se(t=>t.id!==e)}}
                            title="Dismiss" aria-label="Dismiss warning">
                        \u2715
                    </button>
                </div>
                ${!r.value&&f`
                    <div class="msg-warning-text">${t}</div>
                `}
            </div>
        </div>
    `}function Od({text:e}){return f`
        <div class="msg-system">
            ${e}
        </div>
    `}function kd({status:e,error:t}){return!e||e===`completed`?f`<div class="run-boundary run-boundary--completed" />`:f`
        <div class="run-boundary ${e===`failed`?`run-boundary--failed`:e===`cancelled`?`run-boundary--cancelled`:``}">
            <span class="run-boundary-label">${e===`failed`?`run failed`:e===`cancelled`?`run cancelled`:`run ${e}`}</span>
        </div>
        ${e===`failed`&&t&&f`
            <div class="run-boundary-error">${t}</div>
        `}
    `}function Ad({peer:e,reason:t}){return f`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">DM conversation with ${e} ended</span>
            <span class="dm-ended-reason">${t}</span>
        </div>
    `}function jd(e,t){if(!t)return``;switch(e){case`shell`:case`shell_exec`:return t.command?t.command:t.argv?t.argv.join(` `):``;case`fs_read`:return t.path||``;case`fs_write`:return`${t.mode===`append`?`(append) `:``}${t.path||``}`;case`fs_list`:return t.path||`.`;case`workspace_write`:return`${t.file||``}: ${(t.content||``).slice(0,60)}`;case`http_get`:if(!t.url)return``;try{return new URL(t.url).hostname+` `+t.url}catch{return t.url}case`math`:return t.operation?t.operation+`(`+[t.a,t.b,t.n].filter(e=>e!==void 0).join(`, `)+`)`:``;case`echo`:return t.message||t.text||``;case`send_message`:return t.to?`to ${t.to}`:``;case`invoke_agent`:{let e=t.name||t.subagent_name||``,n=t.task||``;return e&&n?`${e}: ${n.length>60?n.slice(0,60)+`…`:n}`:e}case`read_session`:return(t.session_id?t.session_id.slice(0,8)+`…`:``)+(t.last_n?` (last ${t.last_n})`:``);case`read_subagent_session`:return(t.name||``)+(t.last_n?` (last ${t.last_n})`:``);case`list_agents`:case`list_my_sessions`:return``;case`read_messages`:return t.from?`from ${t.from}`:``;case`ignore_message`:return t.from?`from ${t.from}`:``;default:{let e=Object.entries(t);return e.map(([t,n])=>{let r=typeof n==`string`?n:JSON.stringify(n);return e.length>1?`${t}=${r}`:r}).join(` `)}}}function Md(e){return e<1024?e+` B`:e<1024*1024?(e/1024).toFixed(1)+` KB`:(e/(1024*1024)).toFixed(1)+` MB`}var Nd=2e3,Pd=800;function Fd(e){if(!e)return``;let t=e.replace(/\\/g,`/`).split(`/`).filter(Boolean);return t.length<=2?t.join(`/`):`…/`+t.slice(-2).join(`/`)}function Q(e){return typeof e==`object`&&!!e&&typeof e.error==`string`}function Id(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.task_id==`string`?e.task_id:null,n=typeof e.status==`string`?e.status:null,r=typeof e.command==`string`?e.command:null,i=typeof e.exit_code==`number`?e.exit_code:null,a=typeof e.stdout==`string`?e.stdout:``,o=typeof e.stderr==`string`?e.stderr:``,s=typeof e.error==`string`?e.error:null,c=typeof e.message==`string`?e.message:null;return t&&(n===`submitted`||n===`unknown`||n===`not_found_or_still_running`)?f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Status</div>
                <div class="tc-status-row">
                    <span class="tc-kv-badge">${n}</span>
                    <span class="tc-kv-mono">task_id: ${t}</span>
                </div>
                ${c&&f`
                    <pre class="tc-detail-content">${c}</pre>
                `}
            </div>
        `:t&&n===`failed`&&s?f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Status</div>
                <div class="tc-status-row">
                    <span class="tc-kv-badge tc-kv-badge-fail">${n}</span>
                    ${r&&f`<span class="tc-kv-mono">${r}</span>`}
                </div>
            </div>
            <div class="tc-detail-section">
                <div class="tc-detail-label">Error</div>
                <pre class="tc-detail-content tc-detail-error">${s}</pre>
            </div>
        `:i==null&&!a&&!o?null:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Status</div>
            <div class="tc-status-row">
                <span class="${i===0||i==null?`tc-kv-badge`:`tc-kv-badge tc-kv-badge-fail`}">
                    exit ${i??`?`}
                </span>
                ${t&&f`<span class="tc-kv-mono">task_id: ${t}</span>`}
            </div>
        </div>
        ${a&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">stdout</div>
                <pre class="tc-detail-content tc-code-block">${a}</pre>
            </div>
        `}
        ${o&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">stderr</div>
                <pre class="tc-detail-content tc-code-block tc-detail-warn">${o}</pre>
            </div>
        `}
    `}function Ld(e,t){if(typeof e!=`object`||!e||Q(e)||typeof e.content!=`string`)return null;let n=t&&t.path||``,r=typeof e.lines_returned==`number`?e.lines_returned:null,i=typeof e.total_lines==`number`?e.total_lines:null,a=e.has_more_before===!0,o=e.has_more_after===!0,s=typeof e.note==`string`?e.note:null,c=e.byte_budget_exceeded===!0,l=e.line_truncated===!0,u=[];r!=null&&i!=null?r===i?u.push(`${r} lines (full file)`):u.push(`${r} of ${i} lines`):r!=null&&u.push(`${r} lines`),a&&u.push(`more before`),o&&u.push(`more after`),c&&u.push(`byte-budget exceeded`),l&&u.push(`per-line truncated`);let d=u.join(` · `),p=e.content||``;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">
                ${n?Fd(n):`File content`}
            </div>
            ${p?f`<pre class="tc-detail-content tc-code-block">${p}</pre>`:f`<pre class="tc-detail-content tc-detail-muted">${s||`(empty)`}</pre>`}
            ${d&&f`<div class="tc-detail-footer">${d}</div>`}
            ${p&&s&&f`<div class="tc-detail-footer">${s}</div>`}
        </div>
    `}function Rd(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.path==`string`&&e.path||typeof e.file==`string`&&e.file||null,n=typeof e.replacements==`number`?e.replacements:null,r=typeof e.mode==`string`?e.mode:null,i=e.ok===!0;return t?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge ${i?``:`tc-kv-badge-fail`}">
                    ${i?`ok`:`failed`}
                </span>
                <span class="tc-kv-mono">${t}</span>
                ${n!=null&&f`
                    <span class="tc-kv-meta">
                        ${n} ${n===1?`replacement`:`replacements`}
                    </span>
                `}
                ${r&&f`
                    <span class="tc-kv-meta">${r}</span>
                `}
            </div>
        </div>
    `:null}function zd(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.matches))return null;let t=e.matches,n=e.truncated===!0,r=typeof e.truncated_lines==`number`&&e.truncated_lines>0?e.truncated_lines:0;if(t.length===0)return f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Matches</div>
                <div class="tc-detail-footer">No matches found.</div>
            </div>
        `;let i=t[0],a;if(typeof i==`string`)a=f`
            <ul class="tc-match-list">
                ${t.map(e=>f`
                    <li class="tc-match-row tc-match-files">
                        <span class="tc-match-path">${e}</span>
                    </li>
                `)}
            </ul>
        `;else if(i&&typeof i.count==`number`&&typeof i.file==`string`)a=f`
            <ul class="tc-match-list">
                ${t.map(e=>f`
                    <li class="tc-match-row tc-match-count">
                        <span class="tc-match-path">${e.file}</span>
                        <span class="tc-kv-meta">${e.count}</span>
                    </li>
                `)}
            </ul>
        `;else if(i&&typeof i.file==`string`&&typeof i.line==`number`)a=f`
            <ul class="tc-match-list">
                ${t.map(e=>{let t=Array.isArray(e.context_before)?e.context_before:[],n=Array.isArray(e.context_after)?e.context_after:[];return f`
                        <li class="tc-match-row tc-match-content">
                            <div class="tc-match-loc">
                                <span class="tc-match-path">${e.file}</span>
                                <span class="tc-match-sep">:</span>
                                <span class="tc-match-line">${e.line}</span>
                            </div>
                            ${t.length>0&&f`
                                <pre class="tc-match-snippet tc-match-context">${t.join(`
`)}</pre>
                            `}
                            <pre class="tc-match-snippet">${e.content||``}</pre>
                            ${n.length>0&&f`
                                <pre class="tc-match-snippet tc-match-context">${n.join(`
`)}</pre>
                            `}
                        </li>
                    `})}
            </ul>
        `;else return null;let o=typeof e.total_matches==`number`?e.total_matches:null,s=typeof e.total==`number`?e.total:null,c=[];return o==null?s!=null&&c.push(`${s} match${s===1?``:`es`}`):c.push(`${o} match${o===1?``:`es`}`),n&&c.push(`output truncated`),r>0&&c.push(`${r} per-line truncated`),f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Matches</div>
            ${a}
            ${c.length>0&&f`
                <div class="tc-detail-footer">${c.join(` · `)}</div>
            `}
        </div>
    `}function Bd(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.files))return null;let t=e.files,n=typeof e.total==`number`?e.total:t.length,r=e.truncated===!0;if(t.length===0)return f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Files</div>
                <div class="tc-detail-footer">No files matched.</div>
            </div>
        `;let i=[];return n===t.length?i.push(`${n} file${n===1?``:`s`}`):i.push(`${t.length} of ${n} files`),r&&i.push(`output truncated`),f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Files</div>
            <ul class="tc-match-list">
                ${t.map(e=>f`
                    <li class="tc-match-row tc-match-files">
                        <span class="tc-match-path">${e}</span>
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">${i.join(` · `)}</div>
        </div>
    `}function Vd(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.entries))return null;let t=typeof e.path==`string`?e.path:``,n=e.entries;return n.length===0?f`
            <div class="tc-detail-section">
                <div class="tc-detail-label tc-file-header">${t||`/`}</div>
                <div class="tc-detail-footer">Empty directory.</div>
            </div>
        `:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">${t||`/`}</div>
            <ul class="tc-match-list">
                ${n.map(e=>f`
                    <li class="tc-match-row ${e.is_dir?`tc-match-dir`:`tc-match-file`}">
                        <span class="tc-match-path">
                            ${e.is_dir?`${e.name||``}/`:e.name||``}
                        </span>
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">
                ${n.length} ${n.length===1?`entry`:`entries`}
            </div>
        </div>
    `}function Hd(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.status==`number`?e.status:null,a=typeof e.content_type==`string`?e.content_type:null,o=e.body;if(i==null&&o===void 0)return null;let s=a&&a.toLowerCase().includes(`application/json`),c;if(typeof o==`string`)c=o;else if(o==null)c=``;else try{c=JSON.stringify(o,null,2)}catch{c=String(o)}let l=c.length>Nd,u=r&&!r.value&&l?c.slice(0,Nd)+`…`:c,d=e=>{e.stopPropagation(),r&&(r.value=!r.value)},p=i!=null&&i>=200&&i<400?`tc-kv-badge`:`tc-kv-badge tc-kv-badge-fail`,m=[];if(e.headers&&typeof e.headers==`object`&&!Array.isArray(e.headers)){let t=Object.keys(e.headers).sort();for(let n of t){let t=e.headers[n];if(Array.isArray(t))for(let e of t)m.push([n,typeof e==`string`?e:String(e)]);else typeof t==`string`&&m.push([n,t])}}let h=m.length;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Response</div>
            <div class="tc-status-row">
                ${i!=null&&f`
                    <span class="${p}">${i}</span>
                `}
                ${a&&f`
                    <span class="tc-kv-meta">${a}</span>
                `}
            </div>
        </div>
        ${c&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">
                    Body${s?` (JSON)`:``}
                </div>
                <pre class="tc-detail-content tc-code-block">${u}</pre>
                ${l&&r&&f`
                    <button class="tc-show-more" onClick=${d}>
                        ${r.value?`Show less`:`Show more`}
                    </button>
                `}
            </div>
        `}
        ${h>0&&f`
            <details class="tc-detail-section tc-http-headers">
                <summary class="tc-detail-label tc-http-headers-summary">
                    Headers (${h})
                </summary>
                <ul class="tc-record-list tc-http-headers-list">
                    ${m.map(([e,t])=>f`
                        <li class="tc-record-row tc-http-header-row">
                            <span class="tc-kv-mono tc-http-header-key">${e}</span>
                            <span class="tc-kv-meta tc-http-header-value">${t}</span>
                        </li>
                    `)}
                </ul>
            </details>
        `}
    `}function Ud(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.task_id==`string`?e.task_id:null,a=typeof e.session_id==`string`?e.session_id:null,o=typeof e.response==`string`?e.response:``,s=t&&(t.name||t.subagent_name)||``;if(i)return f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Subagent (background)</div>
                <div class="tc-status-row">
                    ${s&&f`<span class="tc-kv-badge">${s}</span>`}
                    <span class="tc-kv-mono">task_id: ${i}</span>
                </div>
                ${a&&f`
                    <button class="tc-detail-link"
                        type="button"
                        onClick=${e=>{e.stopPropagation(),Lu(a,{logPrefix:`invokeAgentLink`})}}>
                        View full session
                    </button>
                `}
            </div>
        `;if(!o&&!a)return null;let c=o.length>Pd,l=r&&!r.value&&c?o.slice(0,Pd)+`…`:o;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Subagent</div>
            <div class="tc-status-row">
                ${s&&f`<span class="tc-kv-badge">${s}</span>`}
                <span class="tc-kv-meta">completed</span>
            </div>
        </div>
        ${o&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Response</div>
                <pre class="tc-detail-content">${l}</pre>
                ${c&&r&&f`
                    <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),r&&(r.value=!r.value)}}>
                        ${r.value?`Show less`:`Show more`}
                    </button>
                `}
            </div>
        `}
        ${a&&f`
            <div class="tc-detail-section">
                <button class="tc-detail-link"
                    type="button"
                    onClick=${e=>{e.stopPropagation(),Lu(a,{logPrefix:`invokeAgentLink`})}}>
                    View full session
                </button>
            </div>
        `}
    `}function Wd(e){if(typeof e!=`object`||!e||Q(e))return null;let t=e.delivered===!0,n=typeof e.dm_session_id==`string`?e.dm_session_id:null,r=typeof e.note==`string`?e.note:null;return!t&&!n?null:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Delivery</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${t?`delivered`:`pending`}</span>
                ${n&&f`
                    <span class="tc-kv-mono">session: ${n}</span>
                `}
            </div>
            ${r&&f`<div class="tc-detail-footer">${r}</div>`}
        </div>
    `}function Gd(e,t,n){let r=n&&n.tool;if(typeof e!=`object`||!e||Q(e))return null;let i=Array.isArray(e.messages)?e.messages:null,a=typeof e.summary==`string`&&e.summary.length>0?e.summary:null,o=typeof e.peer==`string`?e.peer:null,s=typeof e.subagent==`string`?e.subagent:null,c=typeof e.session_id==`string`?e.session_id:null,l=typeof e.note==`string`&&e.note.length>0?e.note:null,u=typeof e.message_count==`number`?e.message_count:typeof e.fallback_message_count==`number`?e.fallback_message_count:null,d=typeof e.showing==`number`?e.showing:typeof e.fallback_showing==`number`?e.fallback_showing:i?i.length:null;if(!i&&a){let e=[];return u!=null&&e.push(`${u} messages total`),c&&e.push(`session: ${c.slice(0,8)}…`),f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">
                    ${s?`Subagent ${s}`:`Summary`}
                </div>
                <pre class="tc-detail-content">${a}</pre>
                ${e.length>0&&f`
                    <div class="tc-detail-footer">${e.join(` · `)}</div>
                `}
            </div>
        `}let p=Array.isArray(e.fallback_messages)?e.fallback_messages:null,m=i||p;if(!m)return null;let h=o?`Conversation with ${o}`:s?`Subagent ${s}`:r===`read_session`?`Session messages`:`Messages`,g=[];return d!=null&&u!=null&&d<u?g.push(`showing ${d} of ${u}`):u!=null&&g.push(`${u} messages`),c&&g.push(`session: ${c.slice(0,8)}…`),f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">${h}</div>
            <ul class="tc-chat-list">
                ${m.map(e=>{let t=typeof e.from==`string`&&e.from||typeof e.role==`string`&&e.role||`?`,n=typeof e.content==`string`?e.content:``;return f`
                        <li class="tc-chat-row ${t===`you`||t===`user`?`tc-chat-self`:`tc-chat-peer`}">
                            <span class="tc-chat-sender">${t}</span>
                            <pre class="tc-chat-content">${n}</pre>
                        </li>
                    `})}
            </ul>
            ${l&&f`
                <div class="tc-detail-footer">${l}</div>
            `}
            ${g.length>0&&f`
                <div class="tc-detail-footer">${g.join(` · `)}</div>
            `}
        </div>
        ${a&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Summary</div>
                <pre class="tc-detail-content">${a}</pre>
            </div>
        `}
    `}function Kd(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.agents))return null;let t=e.agents;return t.length===0?f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Agents</div>
                <div class="tc-detail-footer">No other agents available.</div>
            </div>
        `:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Agents</div>
            <ul class="tc-record-list">
                ${t.map(e=>f`
                    <li class="tc-record-row">
                        <div class="tc-record-head">
                            <span class="tc-kv-badge">${e.name||`?`}</span>
                            ${e.last_active&&f`
                                <span class="tc-kv-meta">${e.last_active}</span>
                            `}
                        </div>
                        ${e.description&&f`
                            <div class="tc-record-body">${e.description}</div>
                        `}
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">
                ${t.length} ${t.length===1?`agent`:`agents`}
            </div>
        </div>
    `}function qd(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.sessions))return null;let t=e.sessions,n=typeof e.total==`number`?e.total:t.length,r=typeof e.showing==`number`?e.showing:t.length;if(t.length===0)return f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Sessions</div>
                <div class="tc-detail-footer">No sessions found.</div>
            </div>
        `;let i=[];return r<n?i.push(`showing ${r} of ${n}`):i.push(`${n} sessions`),f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Sessions</div>
            <ul class="tc-record-list">
                ${t.map(e=>{let t=typeof e.session_id==`string`?e.session_id.slice(0,8)+`…`:`?`;return f`
                        <li class="tc-record-row">
                            <div class="tc-record-head">
                                ${e.context_type&&f`
                                    <span class="tc-kv-badge">${e.context_type}</span>
                                `}
                                <span class="tc-kv-mono">${t}</span>
                                ${typeof e.message_count==`number`&&f`
                                    <span class="tc-kv-meta">
                                        ${e.message_count} msg${e.message_count===1?``:`s`}
                                    </span>
                                `}
                                ${e.last_activity&&f`
                                    <span class="tc-kv-meta">${e.last_activity}</span>
                                `}
                            </div>
                            ${e.source_label&&f`
                                <div class="tc-record-body">${e.source_label}</div>
                            `}
                            ${e.summary&&f`
                                <div class="tc-record-body tc-record-summary">
                                    ${e.summary}
                                </div>
                            `}
                        </li>
                    `})}
            </ul>
            <div class="tc-detail-footer">${i.join(` · `)}</div>
        </div>
    `}function Jd(e){if(Q(e))return null;let t;if(typeof e==`string`)t=e;else if(e&&typeof e==`object`)try{t=JSON.stringify(e,null,2)}catch{return null}else if(typeof e==`number`||typeof e==`boolean`)t=String(e);else return null;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Echoed</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `}function Yd(e){return Q(e)||typeof e!=`number`?null:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${e}</span>
            </div>
        </div>
    `}function Xd(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.iso==`string`?e.iso:null,n=typeof e.human==`string`?e.human:null,r=typeof e.timezone==`string`?e.timezone:null,i=typeof e.local_iso==`string`?e.local_iso:null,a=typeof e.local_human==`string`?e.local_human:null,o=typeof e.local_timezone==`string`?e.local_timezone:null,s=typeof e.utc_offset==`string`?e.utc_offset:null;return!t&&!i?null:f`
        ${(t||n)&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">UTC</div>
                <div class="tc-status-row">
                    ${r&&f`<span class="tc-kv-badge">${r}</span>`}
                    ${n&&f`<span class="tc-kv-meta">${n}</span>`}
                </div>
                ${t&&f`<pre class="tc-detail-content">${t}</pre>`}
            </div>
        `}
        ${(i||a)&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Local</div>
                <div class="tc-status-row">
                    ${o&&f`<span class="tc-kv-badge">${o}</span>`}
                    ${s&&f`<span class="tc-kv-meta">${s}</span>`}
                    ${a&&f`<span class="tc-kv-meta">${a}</span>`}
                </div>
                ${i&&f`<pre class="tc-detail-content">${i}</pre>`}
            </div>
        `}
    `}function Zd(e){if(typeof e!=`object`||!e||Q(e)||e.ignored!==!0)return null;let t=typeof e.reason==`string`?e.reason:``;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Ignored</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">ignored</span>
                ${t&&f`<span class="tc-kv-meta">${t}</span>`}
            </div>
        </div>
    `}function Qd(e,t,n,r){if(t==null)return null;switch(e){case`shell`:case`shell_exec`:return Id(t);case`fs_read`:return Ld(t,n);case`fs_write`:case`workspace_write`:case`fs_edit`:return Rd(t);case`fs_grep`:return zd(t);case`fs_glob`:return Bd(t);case`fs_list`:return Vd(t);case`http_get`:return Hd(t,n,r);case`invoke_agent`:return Ud(t,n,r);case`send_message`:return Wd(t);case`read_messages`:case`read_session`:case`read_subagent_session`:return Gd(t,n,{...r,tool:e});case`list_agents`:return Kd(t);case`list_my_sessions`:return qd(t);case`ignore_message`:return Zd(t);case`echo`:return Jd(t);case`math`:return Yd(t);case`datetime`:return Xd(t);default:return null}}var $d=500,ef=200;function tf(e,t){return typeof e==`string`?e.length<=t?e:e.slice(0,t)+`…`:``}var nf={fs_edit:[`old_string`,`new_string`],invoke_agent:[`task`],send_message:[`message`],ignore_message:[`reason`],echo:[`message`,`text`]};function rf(e,t){if(!t||typeof t!=`object`)return!1;let n=nf[e];if(!n)return!1;for(let e of n){let n=t[e];if(typeof n==`string`&&n.length>ef)return!0}return!1}function af(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function of(e){if(e==null)return``;if(typeof e==`string`)try{let t=JSON.parse(e);return JSON.stringify(t,null,2)}catch{return e}return JSON.stringify(e,null,2)}function sf(e){if(e==null)return 0;let t=typeof e==`string`?e:JSON.stringify(e);return new Blob([t]).size}function cf(e){switch(e){case`shell`:case`shell_exec`:return`$`;case`fs_read`:return`R`;case`fs_write`:return`W`;case`fs_list`:return`L`;case`workspace_write`:return`W`;case`http_get`:return`H`;case`send_message`:return`DM`;case`invoke_agent`:return`IA`;case`read_session`:case`read_subagent_session`:return`RS`;case`list_agents`:return`LA`;case`list_my_sessions`:return`LS`;case`read_messages`:return`RM`;case`ignore_message`:return`IG`;case`math`:return`#`;case`echo`:return`E`;default:return`T`}}function lf(e,t){if(!t)return null;switch(e){case`shell`:case`shell_exec`:if(t.command)return f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Command</div>
                        <pre class="tc-detail-content tc-code-block">${t.command}</pre>
                    </div>
                `;break;case`fs_read`:{let e=typeof t.offset==`number`?t.offset:null,n=typeof t.limit==`number`?t.limit:null,r=e!=null||n!=null?n==null?`from line ${(e||0)+1}`:`lines ${(e||0)+1}–${(e||0)+n}`:null;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||``}</div>
                    ${r&&f`
                        <div class="tc-status-row">
                            <span class="tc-kv-meta">${r}</span>
                        </div>
                    `}
                </div>
            `}case`fs_write`:return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||``}</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">
                            ${t.mode===`append`?`append`:`overwrite`}
                        </span>
                    </div>
                </div>
                ${t.content&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${t.content}</pre>
                    </div>
                `}
            `;case`fs_edit`:{let e=t.replace_all===!0;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||``}</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">
                            ${e?`replace all`:`replace once`}
                        </span>
                    </div>
                </div>
                ${t.old_string&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Find</div>
                        <pre class="tc-detail-content tc-code-block">${tf(t.old_string,ef)}</pre>
                    </div>
                `}
                ${t.new_string&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Replace with</div>
                        <pre class="tc-detail-content tc-code-block">${tf(t.new_string,ef)}</pre>
                    </div>
                `}
            `}case`fs_list`:return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||`.`}</div>
                </div>
            `;case`fs_grep`:{let e=typeof t.output_mode==`string`?t.output_mode:`files_with_matches`,n=e!==`files_with_matches`;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Pattern</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-mono">${t.pattern||``}</span>
                        ${t.path&&f`
                            <span class="tc-kv-meta">in</span>
                            <span class="tc-kv-mono">${t.path}</span>
                        `}
                    </div>
                    ${(n||t.glob||t.case_insensitive)&&f`
                        <div class="tc-status-row">
                            ${n&&f`<span class="tc-kv-badge">${e}</span>`}
                            ${t.glob&&f`<span class="tc-kv-meta">glob: ${t.glob}</span>`}
                            ${t.case_insensitive&&f`<span class="tc-kv-meta">case-insensitive</span>`}
                        </div>
                    `}
                </div>
            `}case`fs_glob`:return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Pattern</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-mono">${t.pattern||``}</span>
                        ${t.path&&f`
                            <span class="tc-kv-meta">in</span>
                            <span class="tc-kv-mono">${t.path}</span>
                        `}
                    </div>
                </div>
            `;case`invoke_agent`:{let e=t.name||t.subagent_name||``,n=t.background===!0;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Agent</div>
                    <div class="tc-status-row">
                        ${e?f`<span class="tc-kv-badge">${e}</span>`:f`<span class="tc-kv-meta">(ephemeral)</span>`}
                        ${n&&f`<span class="tc-kv-meta">background</span>`}
                    </div>
                </div>
                ${t.task&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Task</div>
                        <pre class="tc-detail-content">${tf(t.task,ef)}</pre>
                    </div>
                `}
            `}case`http_get`:if(t.url)return f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Request</div>
                        <div class="tc-status-row">
                            <span class="tc-kv-badge">GET</span>
                            <span class="tc-kv-mono">${t.url}</span>
                        </div>
                    </div>
                `;break;case`workspace_write`:{let e=t.mode===`append`?`append`:`write`;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Workspace</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${t.file||``}</span>
                        <span class="tc-kv-meta">${e}</span>
                    </div>
                </div>
                ${t.content&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${t.content}</pre>
                    </div>
                `}
            `}case`send_message`:return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">To</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${t.to||``}</span>
                    </div>
                </div>
                ${t.message&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Message</div>
                        <pre class="tc-detail-content">${tf(t.message,ef)}</pre>
                    </div>
                `}
            `;case`read_messages`:{let e=typeof t.last_n==`number`?t.last_n:null;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Filter</div>
                    <div class="tc-status-row">
                        ${t.from&&f`<span class="tc-kv-meta">from</span><span class="tc-kv-badge">${t.from}</span>`}
                        ${e!=null&&f`<span class="tc-kv-meta">last ${e}</span>`}
                    </div>
                </div>
            `}case`read_session`:{let e=typeof t.session_id==`string`&&t.session_id?t.session_id:null,n=typeof t.last_n==`number`?t.last_n:null,r=t.summary_only===!0;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Session</div>
                    <div class="tc-status-row">
                        ${e&&f`<span class="tc-kv-mono">${e}</span>`}
                        ${r&&f`<span class="tc-kv-badge">summary only</span>`}
                        ${n!=null&&f`<span class="tc-kv-meta">last ${n}</span>`}
                    </div>
                </div>
            `}case`read_subagent_session`:{let e=typeof t.last_n==`number`?t.last_n:null,n=t.summary_only===!0;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Subagent</div>
                    <div class="tc-status-row">
                        ${t.name&&f`<span class="tc-kv-badge">${t.name}</span>`}
                        ${n&&f`<span class="tc-kv-badge">summary only</span>`}
                        ${e!=null&&f`<span class="tc-kv-meta">last ${e}</span>`}
                    </div>
                </div>
            `}case`ignore_message`:{let e=typeof t.reason==`string`&&t.reason.length>0?t.reason:null;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Ignore</div>
                    ${e?f`<pre class="tc-detail-content">${tf(e,ef)}</pre>`:f`<div class="tc-detail-footer">no reason given</div>`}
                </div>
            `}case`list_my_sessions`:{let e=typeof t.limit==`number`?t.limit:null,n=t.include_current===!0;return e==null&&!n?null:f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Filter</div>
                    <div class="tc-status-row">
                        ${e!=null&&f`<span class="tc-kv-meta">limit ${e}</span>`}
                        ${n&&f`<span class="tc-kv-badge">include current</span>`}
                    </div>
                </div>
            `}case`list_agents`:case`datetime`:return null;case`echo`:return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Message</div>
                    <pre class="tc-detail-content">${tf(t.message||t.text||``,ef)}</pre>
                </div>
            `;case`math`:{let e=typeof t.operation==`string`?t.operation:``,n=[t.a,t.b,t.n].filter(e=>e!=null);return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Expression</div>
                    <div class="tc-status-row">
                        ${e&&f`<span class="tc-kv-badge">${e}</span>`}
                        ${n.length>0&&f`
                            <span class="tc-kv-mono">(${n.join(`, `)})</span>
                        `}
                    </div>
                </div>
            `}}let n=of(t);return n?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters</div>
            <pre class="tc-detail-content">${n}</pre>
        </div>
    `:null}function uf({params:e}){let t=of(e);return t?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters (raw)</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `:null}function df({tool:e,params:t,panelRef:n}){let r=u(!1);p(()=>{bd(n?.current,`pre.tc-code-block`)});let i=lf(e,t);return i?rf(e,t)?f`
        ${r.value?f`<${uf} params=${t} />`:i}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),r.value=!r.value}}>
                ${r.value?`Hide raw params`:`View raw params`}
            </button>
        </div>
    `:i:null}function ff({result:e,isFail:t,showFull:n,label:r,blockedTarget:i}){let a=of(e);if(!a)return null;let o=a.length>$d,s=!n.value&&o?a.slice(0,$d)+`…`:a,c=n.value?` tc-detail-expanded`:``;return f`
        ${i&&f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Target</div>
                <pre class="tc-detail-content tc-code-block tc-detail-error">${i}</pre>
            </div>
        `}
        <div class="tc-detail-section">
            <div class="tc-detail-label">${r}</div>
            <pre class="tc-detail-content${c} ${t?`tc-detail-error`:``}">${s}</pre>
            ${o&&!t&&f`
                <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),n.value=!n.value}}>
                    ${n.value?`Show less`:`Show more`}
                </button>
            `}
        </div>
    `}function pf({tool:e,params:t,result:n,isFail:r,isCancelled:i,showFull:a,panelRef:o}){let s=u(!1);if(p(()=>{bd(o?.current,`pre.tc-code-block`)}),n==null&&!r)return null;let c=r&&typeof n==`object`&&n&&typeof n.target==`string`&&n.target.length>0?n.target:null;if(r)return f`<${ff} result=${n} isFail=${!0}
            showFull=${a} label="Error" blockedTarget=${c} />`;if(i)return f`<${ff} result=${n} isFail=${!1}
            showFull=${a} label="Result (cancelled)" />`;let l=Qd(e,n,t,{showFull:a});return l?f`
        ${s.value?f`<${ff} result=${n} isFail=${!1}
                showFull=${a} label="Result (raw)" />`:l}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),s.value=!s.value}}>
                ${s.value?`Hide raw`:`View raw`}
            </button>
        </div>
    `:f`<${ff} result=${n} isFail=${!1}
            showFull=${a} label="Result" />`}function mf({tool:e,params:t,status:r,result:i,id:a,sourceAgent:o,durationMs:s}){let c=u(!1),l=u(!1),d=e=>{e.stopPropagation(),c.value=!c.value},m=n(null);p(()=>{c.value&&bd(m.current,`pre.tc-code-block`)});let h=jd(e,t),g=h.length>80?h.slice(0,80)+`…`:h,_=r===`running`,v=r===`fail`,y=r===`done`,b=r===`cancelled`,x=e===`send_message`,ee=v?`tc-fail`:y?`tc-done`:b?`tc-cancelled`:`tc-running`,S=c.value?`▼`:`▶`,te=cf(e),ne=af(s),re=i==null?0:sf(i),C=re>=100?Md(re):``;return f`
        <div class="tc-row ${ee} ${x?`tc-dm`:``}" role="button" tabindex="0"
             onClick=${d} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),d(e))}}>
            <div class="tc-header">
                <span class="tc-chevron">${S}</span>
                ${_?f`<span class="tc-spinner"></span>`:f`<span class="tc-icon">${te}</span>`}
                <span class="tc-name">${e}</span>
                ${g&&f`<span class="tc-summary">${g}</span>`}
                <span class="tc-spacer"></span>
                ${C&&f`<span class="tc-result-size">${C}</span>`}
                ${ne&&f`<span class="tc-duration">${ne}</span>`}
                ${v&&f`<span class="tc-status-badge tc-badge-fail">failed</span>`}
                ${b&&f`<span class="tc-status-badge tc-badge-cancelled">cancelled</span>`}
                ${y&&f`<span class="tc-status-icon">\u2713</span>`}
            </div>
            ${c.value&&f`
                <div class="tc-detail" ref=${m}
                     onClick=${e=>e.stopPropagation()}>
                    ${f`<${df} tool=${e} params=${t}
                        panelRef=${m} />`}
                    ${f`<${pf} tool=${e} params=${t}
                        result=${i} isFail=${v} isCancelled=${b}
                        showFull=${l} panelRef=${m} />`}
                </div>
            `}
        </div>
    `}function hf({children:e,count:t}){return t<=1?e:f`
        <div class="tc-group">
            <div class="tc-group-label">${t} tools in parallel</div>
            ${e}
        </div>
    `}function gf(e,t){return e?e.length<=t?e:e.slice(0,t)+`...`:``}function _f(e){switch(e){case`system`:return`cd-role-system`;case`user`:return`cd-role-user`;case`assistant`:return`cd-role-assistant`;case`tool`:return`cd-role-tool`;default:return``}}function vf(e){return e==null?`--`:Number(e).toLocaleString()}function yf({msg:e,index:t}){let n=u(!1),r=e.role||`unknown`,i=e.content||``,a=e.tool_calls&&e.tool_calls.length>0,o=!!e.tool_call_id,s=gf(i,120),c=`[${t}] ${r}`;if(o&&(c+=` (tool_result)`),a){let t=e.tool_calls.map(e=>e.function?.name||`?`).join(`, `);c+=` -> ${t}`}return f`
        <div class="cd-msg" role="button" tabindex="0"
             onClick=${e=>{e.stopPropagation(),n.value=!n.value}}
             onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),e.stopPropagation(),n.value=!n.value)}}>
            <div class="cd-msg-header">
                <span class="cd-msg-chevron">${n.value?`▼`:`▶`}</span>
                <span class="cd-msg-role ${_f(r)}">${r}</span>
                ${!n.value&&s&&f`<span class="cd-msg-preview">${s}</span>`}
            </div>
            ${n.value&&f`
                <div class="cd-msg-body" onClick=${e=>e.stopPropagation()}>
                    ${i&&f`<pre class="cd-msg-content">${i}</pre>`}
                    ${a&&f`
                        <div class="cd-msg-tools">
                            <div class="cd-section-label">Tool calls:</div>
                            ${e.tool_calls.map(e=>f`
                                <pre class="cd-msg-content">${e.function?.name||`?`}(${e.function?.arguments||``})</pre>
                            `)}
                        </div>
                    `}
                </div>
            `}
        </div>
    `}function bf({messages:e,toolNames:t,totalTokens:n,systemTokens:r,historyMessageCount:i,agentName:a,agentId:o}){let s=u(!1),c=e=>{e.stopPropagation(),s.value=!s.value},l=Array.isArray(e)?e.length:0,d=a?`Context sent to LLM (${a})`:`Context sent to LLM`;return f`
        <div class="cd-row" role="button" tabindex="0"
             onClick=${c} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),c(e))}}>
            <div class="cd-header">
                <span class="cd-chevron">${s.value?`▼`:`▶`}</span>
                <span class="cd-icon">CTX</span>
                <span class="cd-title">${d}</span>
                <span class="cd-stats">
                    ${vf(n)} tokens | ${l} messages | ${(t||[]).length} tools
                </span>
            </div>
            ${s.value&&f`
                <div class="cd-detail" onClick=${e=>e.stopPropagation()}>
                    <!-- Token breakdown -->
                    <div class="cd-section">
                        <div class="cd-section-label">Token breakdown</div>
                        <div class="cd-token-grid">
                            <span class="cd-token-label">System prompt:</span>
                            <span class="cd-token-value">${vf(r)}</span>
                            <span class="cd-token-label">History messages:</span>
                            <span class="cd-token-value">${i}</span>
                            <span class="cd-token-label">Total estimated:</span>
                            <span class="cd-token-value cd-token-total">${vf(n)}</span>
                        </div>
                    </div>

                    <!-- Tools available -->
                    <div class="cd-section">
                        <div class="cd-section-label">Tools available (${(t||[]).length})</div>
                        <div class="cd-tool-list">
                            ${(t||[]).map(e=>f`<span class="cd-tool-tag">${e}</span>`)}
                        </div>
                    </div>

                    <!-- Messages -->
                    <div class="cd-section">
                        <div class="cd-section-label">Messages (${l})</div>
                        <div class="cd-messages">
                            ${(e||[]).map((e,t)=>f`
                                <${yf} key=${t} msg=${e} index=${t} />
                            `)}
                        </div>
                    </div>
                </div>
            `}
        </div>
    `}async function xf(e,t){try{await m(`/approvals/${e}`,{decision:t})}catch(e){throw console.error(`[resolveApproval] failed:`,e),e}}function Sf({approvalId:e,tool:t,params:n}){let r=u(!1),i=async()=>{if(!r.value){r.value=!0;try{await xf(e,`approve`)}catch{r.value=!1}}},a=async()=>{if(!r.value){r.value=!0;try{await xf(e,`deny`)}catch{r.value=!1}}},o=r.value;return f`
        <div class="approval-card">
            <h3>\u26a0 Approval required \u2014 ${t}</h3>
            <pre>${JSON.stringify(n,null,2)}</pre>
            <div class="approval-btns">
                <button class="btn btn-approve" onClick=${i} disabled=${o}>
                    ${o?`Submitting...`:`Approve`}
                </button>
                <button class="btn btn-deny" onClick=${a} disabled=${o}>
                    ${o?`Submitting...`:`Deny`}
                </button>
            </div>
        </div>
    `}function Cf(e){return typeof e==`string`&&e.endsWith(`...`)}function wf({runId:e,summary:t,truncated:n}={}){return e?typeof n==`boolean`?n:Cf(t):!1}function Tf({jobSessionUuid:e}={}){return typeof e==`string`&&e.length>0?e:null}function Ef(e,t){return typeof t==`string`&&t.length>0?t:e||``}var Df=150;function Of(e){if(!e)return``;try{return new Date(e).toLocaleTimeString(void 0,{hour:`2-digit`,minute:`2-digit`})}catch{return``}}function kf(e){switch(e){case`success`:return`Completed`;case`error`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Finished`}}function Af(e){switch(e){case`success`:return`✓`;case`error`:return`✗`;case`cancelled`:return`–`;default:return`•`}}function jf({jobName:e,status:t,summary:n,ts:r,runId:i,truncated:a,jobSessionUuid:o,jobSessionId:c}){let l=u(!1),d=u(null),m=u(!1),h=n&&n.length>Df,g=!h||l.value;p(()=>{if(!l.value||d.value!==null||m.value||!wf({runId:i,summary:n,truncated:a}))return;m.value=!0;let e=!1;return jl(i).then(t=>{e||(d.value=Ef(n,t&&t.response))}).catch(()=>{e||(d.value=n||``)}).finally(()=>{e||(m.value=!1)}),()=>{e=!0}},[l.value,i,n,a]);let _=d.value==null?n:d.value,v=`job-card--${t||`success`}`,y=Of(r),b=Af(t),x=kf(t),ee=()=>{l.value=!l.value},S=Tf({jobSessionUuid:o}),te=e=>{e.stopPropagation(),S&&Lu(S,{logPrefix:`job-card`})},ne=_?s(_):``;return f`
        <div class="job-card ${v}">
            <div class="job-card-header">
                <span class="job-card-icon">${b}</span>
                <span class="job-card-badge">${x}</span>
                <span class="job-card-label">Scheduled Job</span>
                ${y&&f`<span class="job-card-time">${y}</span>`}
            </div>
            <div class="job-card-name">${e||`unnamed job`}</div>
            ${n&&f`
                <div class="job-card-body">
                    ${g?f`<div class="job-card-summary markdown-body"
                                     dangerouslySetInnerHTML=${{__html:ne}} />`:f`<div class="job-card-summary-truncated">
                                ${n.slice(0,Df)}...
                            </div>`}
                </div>
            `}
            ${(h||S)&&f`
                <div class="job-card-actions">
                    ${h&&f`
                        <button class="job-card-toggle" onClick=${ee}>
                            ${l.value?`Show less`:`Show more`}
                        </button>
                    `}
                    ${S&&f`
                        <button class="job-card-goto" onClick=${te}>
                            Go to job session →
                        </button>
                    `}
                </div>
            `}
        </div>
    `}var Mf=200;function Nf(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function Pf(e){switch(e){case`done`:return`Completed`;case`fail`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Completed`}}function Ff(e){switch(e){case`done`:return`✓`;case`fail`:return`✗`;case`cancelled`:return`–`;default:return`✓`}}function If({name:e,task:t,status:n,toolCount:r,durationMs:i,sessionId:a,summary:o}){let s=u(!1),c=`sa-card--${n||`done`}`,l=Ff(n),d=Pf(n),p=Nf(i),m=o&&o.length>Mf,h=!s.value&&m?o.slice(0,Mf)+`…`:o;return f`
        <div class="sa-card ${c}">
            <div class="sa-card-header">
                <span class="sa-card-icon">${l}</span>
                <span class="sa-card-badge">${d}</span>
                <span class="sa-card-label">Subagent</span>
                ${p&&f`<span class="sa-card-meta">${p}</span>`}
                ${r>0&&f`<span class="sa-card-meta">${r} tool${r===1?``:`s`}</span>`}
            </div>
            <div class="sa-card-name">${e||`subagent`}</div>
            ${t&&f`<div class="sa-card-task">${t}</div>`}
            ${o&&f`
                <div class="sa-card-body">
                    <div class="sa-card-summary">${h}</div>
                    ${m&&f`
                        <button class="sa-card-toggle" onClick=${()=>{s.value=!s.value}}>
                            ${s.value?`Show less`:`Show more`}
                        </button>
                    `}
                </div>
            `}
            ${a&&f`
                <div class="sa-card-actions">
                    <button class="sa-card-view-btn" onClick=${e=>{e.stopPropagation(),a&&yc(a)}}>
                        View session \u2192
                    </button>
                </div>
            `}
        </div>
    `}function Lf(e){let t=w.value.filter((t,n)=>n!==e);w.value=t,fe(x.value,t)}function Rf(){let e=w.value;return e.length===0?null:f`
        <div id="message-queue">
            ${e.map((e,t)=>f`
                <div class="queued-msg">
                    <span class="queued-msg-label">queued</span>
                    <span class="queued-msg-text">${e.text}</span>
                    <button class="queued-msg-remove" title="Remove from queue"
                            onClick=${()=>Lf(t)}>\u00d7</button>
                </div>
            `)}
        </div>
    `}var zf=e({InputArea:()=>Wf,startRun:()=>Bf});async function Bf(e,t){let n=t?.sessionId||x.value,r=O.value;if(!r){N(e=>[...e,{id:P(),type:`error`,text:`Select an agent before sending a message.`}]);return}I({id:P(),type:`user`,role:`user`,text:e,ts:new Date().toISOString()},{id:P(),type:`thinking`,pending:!0}),n&&me(n,e);try{let t=await Al({session_id:n,agent_id:r,input:{type:`text`,text:e}});n&&t?.run_id&&ne(n,t.run_id)}catch(e){n&&te(n),N(t=>[...t.filter(e=>e.type!==`thinking`),{id:P(),type:`error`,text:`Failed to start run: ${e.error?.message||e.message||e.status||`unknown error`}`}]),console.error(`[startRun] failed:`,e)}}function Vf(e){let t=e.current.value.trim();if(!t||!x.value||!O.value)return;let n=x.value;if(e.current.value=``,e.current.style.height=`auto`,ue(n),M.value){let r=[...w.value,{text:t}];w.value=r,fe(n,r),e.current.focus();return}Bf(t)}async function Hf(){if(M.value)try{await Nl(M.value)}catch{}}function Uf(e){e.style.height=`auto`,e.style.height=Math.min(e.scrollHeight,150)+`px`}function Wf(){let e=n(null),t=k.value.length>0,r=!!x.value,i=!!O.value,a=t&&i&&r,o=!!M.value,s=i?`Send a message...`:`Select an agent to send a message`,c=x.value;return p(()=>{let t=e.current;t&&(t.value=T(c),Uf(t));let n=ae(c),r=ee({restoredQueue:n,activeRunId:M.value,activeAgentId:O.value});n.length>0&&(w.value=n),r.drain&&(w.value=r.remaining,fe(c,r.remaining),Bf(r.head.text,{sessionId:c}))},[c]),f`
        <div id="input-area">
            <div class="input-container">
                <textarea id="prompt" ref=${e} rows="1"
                          placeholder=${s}
                          aria-label="Message input"
                          disabled=${!a}
                          onKeyDown=${t=>{t.key===`Enter`&&!t.shiftKey&&(t.preventDefault(),Vf(e))}}
                          onInput=${()=>{let t=e.current;t&&(Uf(t),de(c,t.value))}}></textarea>
                ${o?f`<button id="cancel-run" title="Stop run" aria-label="Stop run"
                                   onClick=${Hf}><${Tu} /></button>`:f`<button id="send" disabled=${!a}
                                   title="Send (Enter)" aria-label="Send message"
                                   onClick=${()=>Vf(e)}><${wu} /></button>`}
            </div>
        </div>
    `}var Gf=()=>v(`/agents`),Kf=e=>m(`/agents`,e),qf=(e,t)=>oe(`/agents/${e}`,t),Jf=e=>g(`/agents/${e}`),Yf=e=>m(`/agents/${e}/default`),Xf={"claude-opus-4-7":{name:`Claude Opus 4.7`,provider:`anthropic`},"claude-opus-4-6":{name:`Claude Opus 4.6`,provider:`anthropic`},"claude-sonnet-4-6":{name:`Claude Sonnet 4.6`,provider:`anthropic`},"claude-sonnet-4-5":{name:`Claude Sonnet 4.5`,provider:`anthropic`},"claude-haiku-4-5":{name:`Claude Haiku 4.5`,provider:`anthropic`},"gpt-5.4":{name:`GPT-5.4`,provider:`openai`},"gpt-5.4-mini":{name:`GPT-5.4 mini`,provider:`openai`},"gpt-5.4-nano":{name:`GPT-5.4 nano`,provider:`openai`},"gpt-4.1":{name:`GPT-4.1`,provider:`openai`},"gpt-4.1-mini":{name:`GPT-4.1 mini`,provider:`openai`},"gpt-4.1-nano":{name:`GPT-4.1 nano`,provider:`openai`},"gpt-4o":{name:`GPT-4o`,provider:`openai`},"gpt-4o-mini":{name:`GPT-4o mini`,provider:`openai`},"o4-mini":{name:`o4-mini`,provider:`openai`},o3:{name:`o3`,provider:`openai`},"o3-mini":{name:`o3-mini`,provider:`openai`},"grok-4.20":{name:`Grok 4.20`,provider:`xai`},"grok-4-fast":{name:`Grok 4 Fast`,provider:`xai`},"grok-3":{name:`Grok 3`,provider:`xai`},"grok-3-mini":{name:`Grok 3 mini`,provider:`xai`},"deepseek-chat":{name:`DeepSeek Chat (V3)`,provider:`deepseek`},"deepseek-reasoner":{name:`DeepSeek Reasoner (R1)`,provider:`deepseek`},"mistral-large-latest":{name:`Mistral Large`,provider:`mistral`},"mistral-medium-latest":{name:`Mistral Medium`,provider:`mistral`},"mistral-small-latest":{name:`Mistral Small`,provider:`mistral`},"codestral-latest":{name:`Codestral`,provider:`mistral`},"ministral-8b-latest":{name:`Ministral 8B`,provider:`mistral`},"open-mistral-nemo":{name:`Mistral Nemo`,provider:`mistral`},"llama-3.3-70b-versatile":{name:`Llama 3.3 70B`,provider:`groq`},"llama-3.1-8b-instant":{name:`Llama 3.1 8B (Instant)`,provider:`groq`},"deepseek-r1-distill-llama-70b":{name:`DeepSeek R1 Distill (70B)`,provider:`groq`},"qwen-2.5-32b":{name:`Qwen 2.5 32B`,provider:`groq`},"qwen2.5-coder:32b":{name:`Qwen 2.5 Coder 32B`,provider:`ollama`},"deepseek-r1:7b":{name:`DeepSeek R1 7B`,provider:`ollama`},"llama3.3:70b":{name:`Llama 3.3 70B`,provider:`ollama`},"deepseek/deepseek-r1":{name:`DeepSeek R1`,provider:`openrouter`},"deepseek/deepseek-chat-v3-0324":{name:`DeepSeek Chat v3`,provider:`openrouter`},"z-ai/glm-5.2":{name:`GLM 5.2`,provider:`openrouter`},"z-ai/glm-5.1":{name:`GLM 5.1`,provider:`openrouter`},"minimax/minimax-m2.7":{name:`MiniMax M2.7`,provider:`openrouter`},"xiaomi/mimo-v2-pro":{name:`MiMo v2-pro`,provider:`openrouter`},"moonshotai/kimi-k2.6":{name:`Kimi K2.6`,provider:`openrouter`},"google/gemma-4-31b-it":{name:`Gemma 4 31B`,provider:`openrouter`}},Zf=`claude-opus-4-7,claude-sonnet-4-6,claude-haiku-4-5,claude-opus-4-6,gpt-5.4,gpt-5.4-mini,gpt-5.4-nano,gpt-4.1,gpt-4.1-mini,gpt-4o,gpt-4o-mini,o4-mini,o3,grok-4.20,grok-4-fast,grok-3-mini,deepseek-chat,deepseek-reasoner,mistral-large-latest,mistral-small-latest,codestral-latest,llama-3.3-70b-versatile,llama-3.1-8b-instant,deepseek-r1-distill-llama-70b,qwen2.5-coder:32b,deepseek-r1:7b,llama3.3:70b,z-ai/glm-5.2,deepseek/deepseek-r1,deepseek/deepseek-chat-v3-0324,z-ai/glm-5.1,minimax/minimax-m2.7,xiaomi/mimo-v2-pro,moonshotai/kimi-k2.6,google/gemma-4-31b-it`.split(`,`);function Qf(e){if(!e)return``;let t=Xf[e];return t?t.name:e}function $f(e){if(!e)return`unknown`;let t=Xf[e];return t?t.provider:e.includes(`/`)?`openrouter`:e.includes(`:`)?`ollama`:e.startsWith(`claude`)?`anthropic`:e.startsWith(`gpt`)||/^o\d/.test(e)?`openai`:e.startsWith(`grok`)?`xai`:e.startsWith(`deepseek-`)?`deepseek`:e.startsWith(`mistral-`)||e.startsWith(`codestral-`)||e.startsWith(`ministral-`)||e.startsWith(`open-mistral-`)||e.startsWith(`open-mixtral-`)?`mistral`:e.startsWith(`llama-`)?`groq`:e.startsWith(`gemini-`)?`google`:`unknown`}var ep={anthropic:`Anthropic`,openai:`OpenAI`,openrouter:`OpenRouter`,xai:`xAI`,deepseek:`DeepSeek`,mistral:`Mistral`,groq:`Groq`,ollama:`Ollama`,google:`Google`,unknown:`Custom`};function tp(e){return e?ep[e]?ep[e]:ep[$f(e)]:ep.unknown}function np({modelId:e,provider:t}){let n=t||$f(e),r=ep[n]||ep.unknown;return f`
        <span class="model-provider-badge model-provider-badge--${n}"
              title=${`Provider: ${r}`}>${r}</span>
    `}function rp({value:e,defaultValue:t,showBadge:n=!0}){let r=e&&e.trim?e.trim():e,i=!!r&&r!==t,a=r||t;if(!a)return f`<span class="model-display model-display--muted">unknown</span>`;let o=Qf(a),s=o===a?a:`${o} (${a})`;return i?f`
            <span class="model-display" title=${s}>
                <span class="model-override-pill" title="Per-run override">override</span>
                <span class="model-name">${o}</span>
                ${n&&f`<${np} modelId=${a} />`}
            </span>
        `:f`
        <span class="model-display model-display--default" title=${s}>
            <span class="model-default-label">Default</span>
            <span class="model-name">${o}</span>
            ${n&&f`<${np} modelId=${a} />`}
        </span>
    `}function ip({value:e,defaultValue:t}){let n=e&&e.trim?e.trim():e,r=!!n&&n!==t,i=n||t;return i?r?f`
            <span class="model-display">
                <span class="model-override-pill" title="Per-run override">override</span>
                <${np} provider=${i} />
            </span>
        `:f`
        <span class="model-display model-display--default">
            <span class="model-default-label">Default</span>
            <${np} provider=${i} />
        </span>
    `:f`<span class="model-display model-display--muted">unknown</span>`}function ap(e){return e==null?``:String(e).toLowerCase().replace(/\s+/g,`-`).replace(/[^a-z0-9-]/g,``).replace(/-+/g,`-`).replace(/^-+|-+$/g,``)}var op=[`default`,`dm`,`workspace`],sp=/^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32})$/,cp=64;function lp(e){return typeof e!=`string`||e.length===0?null:e.length>cp?{code:`AGENT_NAME_TOO_LONG`,message:`Agent name is too long (max ${cp} characters after normalization)`}:op.includes(e)?{code:`AGENT_NAME_RESERVED`,message:`'${e}' is a reserved name`}:sp.test(e)?{code:`AGENT_NAME_LOOKS_LIKE_UUID`,message:`'${e}' looks like a UUID (conflicts with ID-based lookup)`}:null}function up(e){return e===`Enter`||e===` `}function dp(e){return!e||typeof e.key!=`string`||e.defaultPrevented?!1:up(e.key)}function fp(e){return!(!e||e.defaultPrevented)}function pp(e,t){let n=!!e,r=!!t;return n===r?{}:{debug_mode:r}}var mp=[`minimal`,`low`,`medium`,`high`];function hp(e){return e==null?`inherit`:e===0?`disable`:`custom`}function gp(e){return e==null||e===``?`inherit`:`custom`}function _p({label:e,hint:t,agentValue:n,mode:r,draft:i}){return f`
        <div class="settings-row">
            <label class="settings-label">${e}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${r.value}
                        onChange=${e=>{r.value=e.target.value,r.value!==`custom`&&(i.value=``)}}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="disable">Disable (0)</option>
                    <option value="custom">Custom</option>
                </select>
                ${r.value===`custom`&&f`
                    <input class="settings-input agent-tristate-value"
                           type="number" min="0" step="1024"
                           placeholder="tokens"
                           value=${i.value}
                           onInput=${e=>{i.value=e.target.value}} />
                `}
            </div>
            ${t&&f`<span class="settings-hint">${t}</span>`}
        </div>
    `}function vp({label:e,hint:t,mode:n,value:r}){return f`
        <div class="settings-row">
            <label class="settings-label">${e}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${n.value}
                        onChange=${e=>{n.value=e.target.value,n.value!==`custom`&&(r.value=``)}}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="custom">Custom</option>
                </select>
                ${n.value===`custom`&&f`
                    <select class="settings-select agent-tristate-value"
                            value=${r.value}
                            onChange=${e=>{r.value=e.target.value}}>
                        <option value="">choose...</option>
                        ${mp.map(e=>f`<option value=${e} key=${e}>${e}</option>`)}
                    </select>
                `}
            </div>
            ${t&&f`<span class="settings-hint">${t}</span>`}
        </div>
    `}async function yp(){try{let e=await Gf();k.value=e.agents||e||[]}catch(e){console.error(`[agents] fetch failed:`,e)}}function bp({agent:e,onClose:t}){let n=u(e.description||``),r=u(e.model||``),i=u(e.posture||``),a=u(e.provider||``),o=u(`keep`),s=u(``),c=u(hp(e.thinking_budget_tokens)),l=u(e.thinking_budget_tokens&&e.thinking_budget_tokens>0?String(e.thinking_budget_tokens):``),d=u(gp(e.reasoning_effort)),p=u(e.reasoning_effort||``),m=u(hp(e.gemini_thinking_budget)),h=u(e.gemini_thinking_budget&&e.gemini_thinking_budget>0?String(e.gemini_thinking_budget):``),g=u(e.summary_provider||``),_=u(e.summary_model||``),v=u(!!e.debug_mode),y=u(!1),b=u(``),x=Ts.value.model||``,ee=Ts.value.provider||``,S=Ts.value.llm_providers||[],te=()=>{let t={};n.value!==(e.description||``)&&(t.description=n.value),(r.value||``)!==(e.model||``)&&(t.model=r.value||``),(i.value||``)!==(e.posture||``)&&(t.posture=i.value||``),(a.value||``)!==(e.provider||``)&&(t.provider=a.value||``),o.value===`set`&&s.value.trim()?t.telegram_token=s.value.trim():o.value===`remove`&&(t.telegram_token=``);let u=e.thinking_budget_tokens;if(c.value===`inherit`)u!=null&&(t.clear_thinking_budget_tokens=!0);else if(c.value===`disable`)u!==0&&(t.thinking_budget_tokens=0);else if(c.value===`custom`){let e=parseInt(l.value,10);!isNaN(e)&&e>=0&&e!==u&&(t.thinking_budget_tokens=e)}let f=e.reasoning_effort||null;d.value===`inherit`?f!=null&&(t.clear_reasoning_effort=!0):d.value===`custom`&&p.value&&p.value!==f&&(t.reasoning_effort=p.value);let y=e.gemini_thinking_budget;if(m.value===`inherit`)y!=null&&(t.clear_gemini_thinking_budget=!0);else if(m.value===`disable`)y!==0&&(t.gemini_thinking_budget=0);else if(m.value===`custom`){let e=parseInt(h.value,10);!isNaN(e)&&e>=0&&e!==y&&(t.gemini_thinking_budget=e)}let b=e.summary_provider||``,x=(g.value||``).trim();x!==b&&(x===``?t.clear_summary_provider=!0:t.summary_provider=x);let ee=e.summary_model||``,S=(_.value||``).trim();return S!==ee&&(S===``?t.clear_summary_model=!0:t.summary_model=S),Object.assign(t,pp(e.debug_mode,v.value)),t},ne=async()=>{y.value=!0,b.value=``;try{let n=te();if(Object.keys(n).length===0){t();return}await qf(e.id,n),await yp(),t()}catch(e){b.value=e.error?.message||e.message||`Save failed`}finally{y.value=!1}},re=e=>{e.target===e.currentTarget&&t()},C=!!e.has_telegram;return f`
        <div class="settings-overlay open" onClick=${re}>
            <div class="settings-modal agent-edit-modal">
                <h2>${e.name}</h2>

                <div class="settings-row">
                    <label class="settings-label">Description</label>
                    <input class="settings-input" type="text"
                           placeholder="optional"
                           value=${n.value}
                           onInput=${e=>{n.value=e.target.value}} />
                </div>

                <div class="settings-row">
                    <label class="settings-label">Model</label>
                    <input class="settings-input" type="text"
                           list="agent-model-suggestions"
                           placeholder=${x||`server default`}
                           value=${r.value}
                           onInput=${e=>{r.value=e.target.value}} />
                    <datalist id="agent-model-suggestions">
                        ${Zf.map(e=>f`<option value=${e} />`)}
                    </datalist>
                    <span class="settings-effective">
                        Effective: <${rp} value=${r.value.trim()} defaultValue=${x} />
                    </span>
                    <span class="settings-hint">Leave empty to use server default.</span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Provider</label>
                    <select class="settings-select"
                            value=${a.value}
                            onChange=${e=>{a.value=e.target.value}}>
                        <option value="">Default (${tp(ee||`openai`)})</option>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="openrouter">OpenRouter</option>
                    </select>
                    <span class="settings-effective">
                        Effective: <${ip} value=${a.value} defaultValue=${ee||`openai`} />
                    </span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Posture</label>
                    <select class="settings-select"
                            value=${i.value}
                            onChange=${e=>{i.value=e.target.value}}>
                        <option value="">Server default (${Ts.value.posture||`guarded`})</option>
                        <option value="full_control">full_control</option>
                        <option value="guarded">guarded</option>
                        <option value="autonomous">autonomous</option>
                    </select>
                </div>

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Reasoning &amp; thinking</div>

                <span class="settings-hint">
                    Provider-specific. Each row applies only when this agent's effective provider matches —
                    Anthropic / OpenAI / Gemini knobs are silently ignored on other providers. Explicit values
                    take effect on the next run.
                </span>

                <${_p}
                    label="Anthropic thinking budget"
                    hint="Inherit = use server default. Disable = Some(0) (force off for this agent). Custom = override with N tokens."
                    agentValue=${e.thinking_budget_tokens}
                    mode=${c}
                    draft=${l} />

                <${vp}
                    label="OpenAI reasoning effort"
                    hint="Inherit = use server default. Custom picks an effort level for this agent."
                    mode=${d}
                    value=${p} />

                <${_p}
                    label="Gemini thinking budget"
                    hint="Inherit = use server default. Disable = Some(0) (force off for this agent). Custom = override with N tokens."
                    agentValue=${e.gemini_thinking_budget}
                    mode=${m}
                    draft=${h} />

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Summary (compact strategy + episodic memory)</div>

                <span class="settings-hint">
                    Per-agent summary provider/model. Drives both the in-loop compact-strategy compaction and the
                    post-run episodic memory generation. Both fields must be set together — partial settings are
                    rejected server-side. Leave both empty to inherit the server default.
                </span>

                <div class="settings-row">
                    <label class="settings-label">Summary provider</label>
                    <select class="settings-select"
                            value=${g.value}
                            onChange=${e=>{g.value=e.target.value}}>
                        <option value="">Use server default</option>
                        ${(S.length>0?S:[`openai`,`anthropic`,`openrouter`,`gemini`]).map(e=>{let t=tp(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                    </select>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Summary model</label>
                    <input class="settings-input" type="text"
                           list="agent-model-suggestions"
                           placeholder="(use server default)"
                           value=${_.value}
                           onInput=${e=>{_.value=e.target.value}} />
                    <span class="settings-hint">
                        Model slug for the summary provider. Set together with Summary provider.
                    </span>
                </div>

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Debug</div>

                <div class="settings-row">
                    <label class="settings-label">Context-window inspection</label>
                    <label class="settings-toggle">
                        <input type="checkbox"
                               checked=${v.value}
                               onChange=${e=>{v.value=e.target.checked}} />
                        <span>${v.value?`enabled`:`disabled`}</span>
                    </label>
                    <span class="settings-hint">
                        When enabled, every turn emits a snapshot of the full assembled context window
                        (system prompts, workspace, episodic memory, history, tool definitions, in the order
                        the runtime sends them). The web UI renders the snapshot in a collapsible panel
                        below the chat. Works for both webchat and DM sessions — for DMs, each turn shows
                        the per-perspective context the agent currently being inspected sees.
                        Per-agent. Takes effect on the next run; previous turns are not retroactively shown.
                    </span>
                </div>

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Telegram bot</div>

                <div class="settings-row">
                    <label class="settings-label">Token</label>
                    <div class="settings-info-row-header" style="flex-wrap:wrap;gap:6px;">
                        <span class="settings-info-row-value">
                            ${C?`configured (token hidden)`:`not configured`}
                        </span>
                        <select class="settings-select agent-tristate-mode"
                                value=${o.value}
                                onChange=${e=>{o.value=e.target.value,e.target.value!==`set`&&(s.value=``)}}>
                            <option value="keep">Keep</option>
                            <option value="set">${C?`Replace`:`Set`}</option>
                            ${C&&f`<option value="remove">Remove</option>`}
                        </select>
                    </div>
                    ${o.value===`set`&&f`
                        <input class="settings-input" type="password"
                               autocomplete="off"
                               placeholder="paste bot token..."
                               value=${s.value}
                               onInput=${e=>{s.value=e.target.value}} />
                    `}
                    <span class="settings-hint">
                        ${C?`A token is set but is never displayed. Replace overwrites it; Remove clears it.`:`Set a bot token to enable a dedicated Telegram polling loop for this agent.`}
                    </span>
                    <span class="settings-hint">
                        Token changes only take effect after the daemon restarts. Tracked in #821.
                    </span>
                </div>

                ${b.value&&f`<div class="inline-error">${b.value}</div>`}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${t}>Cancel</button>
                    <button class="settings-save" onClick=${ne} disabled=${y.value}>
                        ${y.value?`...`:`Save`}
                    </button>
                </div>
            </div>
        </div>
    `}function xp({agent:e,isActive:t,onEdit:n}){let r=u(``),i=u(!1),a=u(null),o=Ts.value.model||``,s=Ts.value.provider||``;return f`
        <div class="agent-card ${t?`active`:``}"
             role="option"
             tabindex="0"
             aria-label=${`Select agent `+e.name}
             aria-selected=${t?`true`:`false`}
             onClick=${t=>{fp(t)&&mu(e.id)}}
             onKeyDown=${t=>{dp(t)&&(t.preventDefault(),mu(e.id))}}>
            <div class="agent-card-header">
                <span class="agent-card-name">${e.name}</span>
                ${e.is_default&&f`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta agent-card-meta--model">
                <span class="agent-card-meta-label">model:</span>
                <${rp} value=${e.model} defaultValue=${o} />
            </div>
            ${e.provider&&f`
                <div class="agent-card-meta agent-card-meta--provider">
                    <span class="agent-card-meta-label">provider:</span>
                    <${ip} value=${e.provider} defaultValue=${s} />
                </div>
            `}
            ${e.posture&&f`
                <div class="agent-card-meta agent-card-meta--posture">
                    <span class="agent-card-meta-label">posture:</span>
                    <span>${e.posture}</span>
                </div>
            `}
            ${r.value&&f`<div class="agent-error">${r.value}</div>`}
            <div class="agent-card-actions">
                <button class="agent-card-btn" onClick=${t=>{t&&t.stopPropagation(),n(e)}}>Edit</button>
                ${!e.is_default&&f`
                    <button class="agent-card-btn" onClick=${async t=>{t&&t.stopPropagation();try{await Yf(e.id),await yp()}catch(e){r.value=e.error?.message||e.message||`Failed`}}}>Set Default</button>
                `}
                ${i.value?f`
                        <button class="agent-card-btn" style="color:var(--error); font-weight:600;" onClick=${async t=>{t&&t.stopPropagation(),a.value&&=(clearTimeout(a.value),null),i.value=!1;try{if(await Jf(e.id),await yp(),e.id===O.value){let e=k.value.find(e=>e.is_default)||k.value[0]||null;e?mu(e.id):(O.value=null,ge.value=null)}}catch(e){r.value=e.error?.message||e.message||`Delete failed`}}}>Confirm?</button>
                        <button class="agent-card-btn" onClick=${e=>{e&&e.stopPropagation(),a.value&&=(clearTimeout(a.value),null),i.value=!1}}>Cancel</button>
                    `:f`<button class="agent-card-btn" style="color:var(--error);" onClick=${e=>{e&&e.stopPropagation(),i.value=!0,a.value=setTimeout(()=>{i.value=!1},3e3)}}>Delete</button>`}
            </div>
        </div>
    `}function Sp(){let e=u(``),t=u(``),n=u(!1),r=u(null);p(()=>{Z.value===`agents`&&yp()},[Z.value]);let i=ap(e.value),a=(e.value||``).trim(),o=a!==``&&i!==a,s=async()=>{let r=ap(e.value);if(!r){(e.value||``).trim()===``?t.value=`Agent name is required`:t.value=`Agent name must contain at least one letter or digit`;return}let i=lp(r);if(i){t.value=i.message;return}t.value=``,n.value=!0;try{let t=await Kf({name:r});t.id||console.warn(`[agents] POST /agents returned no id for agent:`,r,t),e.value=``,await yp()}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}};return f`
        <div class="agent-list-container">
            <div class="agent-create-row">
                <input type="text" placeholder="New agent name..."
                       aria-label="Agent name"
                       value=${e.value}
                       onInput=${t=>{e.value=t.target.value}}
                       onKeyDown=${e=>{e.key===`Enter`&&s()}} />
                <button class="agent-card-btn agent-create-btn" onClick=${s}
                        disabled=${n.value}>
                    ${n.value?`...`:`+ Create`}
                </button>
            </div>
            ${o&&f`
                <div class="agent-create-preview">
                    Will be saved as: <code>${i}</code>
                </div>
            `}

            ${t.value&&f`<div class="agent-error">${t.value}</div>`}

            <div class="agent-list" role="listbox" aria-label="Agents">
                ${k.value.length===0?f`<div class="empty-state">No agents</div>`:k.value.map(e=>f`
                        <${xp} key=${e.id} agent=${e}
                                      isActive=${e.id===O.value}
                                      onEdit=${e=>{r.value=e}} />
                    `)}
            </div>

            ${r.value&&f`
                <${bp}
                    agent=${r.value}
                    onClose=${()=>{r.value=null}} />
            `}
        </div>
    `}var Cp=e=>v(`/agents/${e}/workspace`),wp=(e,t,n)=>oe(`/agents/${e}/workspace/${t}`,{content:n}),Tp=e=>m(`/agents/${e}/workspace/open`,{}),Ep=[`personality`,`goals`,`memories`,`user`];async function Dp(){if(!O.value){Ds.value=null;return}try{let e=await Cp(O.value);Ds.value=e.files||e}catch(e){e.status===404||e.error?.code===`NOT_FOUND`?Ds.value=`unavailable`:Ds.value=`error`}}function Op({agentId:e,doOpen:t}){let n=u(!1),r=u(null),i=async()=>{if(!(n.value||!e)){n.value=!0,r.value=null;try{await t(e),r.value={kind:`ok`,text:`Opened`},setTimeout(()=>{r.value?.kind===`ok`&&(r.value=null)},2e3)}catch(e){let t=e?.error?.code,n=e?.error?.message||e?.message||`Failed to open workspace`,i=n;t===`NOT_CONFIGURED`?i=`Workspace dir not configured`:t===`WORKSPACE_PATH_MISSING`?i=`Workspace path is missing on disk`:t===`LAUNCHER_FAILED`&&(i=`Failed to launch file explorer`),r.value={kind:`err`,text:i,full:n}}finally{n.value=!1}}};return f`
        <div class="ws-open-row">
            <button class="ws-open-btn"
                    type="button"
                    onClick=${i}
                    onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),i())}}
                    disabled=${n.value||!e}
                    title="Open this agent's workspace directory in the host file explorer"
                    aria-label="Open workspace in file explorer">
                ${n.value?`Opening...`:`Open in Explorer`}
            </button>
            ${r.value&&f`
                <span class="ws-flash ${r.value.kind===`ok`?`ok`:`err`}"
                      title=${r.value.full||``}
                      role=${r.value.kind===`err`?`alert`:`status`}>
                    ${r.value.text}
                </span>
            `}
        </div>
    `}function kp({agentId:e,filename:t,content:n}){let r=u(n||``),i=u(``),a=u(!1);return p(()=>{r.value=n||``},[n]),f`
        <div class="ws-file">
            <div class="ws-file-label">${t}</div>
            <textarea class="ws-textarea"
                      rows="6"
                      value=${r.value}
                      onInput=${e=>{r.value=e.target.value}}></textarea>
            <div style="display:flex; align-items:center; gap:var(--space-2);">
                <button class="ws-save" onClick=${async()=>{if(!a.value){a.value=!0,i.value=``;try{await wp(e,t,r.value),i.value=`Saved`,setTimeout(()=>{i.value=``},2e3),await Dp()}catch(e){i.value=`Error: `+(e.error?.message||e.message||`save failed`)}finally{a.value=!1}}}} disabled=${a.value}>
                    ${a.value?`Saving...`:`Save`}
                </button>
                ${i.value&&f`
                    <span class="ws-flash ${i.value.startsWith(`Error`)?`err`:`ok`}">
                        ${i.value}
                    </span>
                `}
            </div>
        </div>
    `}function Ap(){return p(()=>{Z.value===`workspace`&&Dp()},[Z.value,O.value]),O.value?Ds.value===null?f`<div class="loading-state">Loading...</div>`:Ds.value===`unavailable`?f`<div class="ws-notice">Workspace not configured for this agent</div>`:Ds.value===`error`?f`<div class="ws-notice" style="color:var(--error);">Failed to load workspace</div>`:f`
        <div>
            <${Op}
                agentId=${O.value}
                doOpen=${Tp} />
            ${Ep.map(e=>f`
                <${kp}
                    key=${e}
                    agentId=${O.value}
                    filename=${e}
                    content=${Ds.value[e+`.md`]||Ds.value[e]||``} />
            `)}
        </div>
    `:f`<div class="ws-notice">No agent selected</div>`}var jp=d([]),Mp=()=>v(`/jobs`),Np=e=>m(`/jobs`,e),Pp=e=>g(`/jobs/${e}`),Fp=[{label:`1m`,cron:`* * * * *`,desc:`Every minute`},{label:`5m`,cron:`*/5 * * * *`,desc:`Every 5 minutes`},{label:`15m`,cron:`*/15 * * * *`,desc:`Every 15 minutes`},{label:`30m`,cron:`*/30 * * * *`,desc:`Every 30 minutes`},{label:`1h`,cron:`0 * * * *`,desc:`Every hour`},{label:`6h`,cron:`0 */6 * * *`,desc:`Every 6 hours`},{label:`12h`,cron:`0 */12 * * *`,desc:`Every 12 hours`},{label:`1d`,cron:`0 0 * * *`,desc:`Daily at midnight`}];function Ip(e){if(!e)return``;let t=Fp.find(t=>t.cron===e.trim());return t?t.desc:e.trim().split(/\s+/).length===5?e:`Invalid cron (need 5 fields)`}function Lp(e){let t=e=>String(e).padStart(2,`0`);return`${e.getFullYear()}-${t(e.getMonth()+1)}-${t(e.getDate())}T${t(e.getHours())}:${t(e.getMinutes())}`}function Rp(){let e=new Date(Date.now()+5*6e4);return e.setSeconds(0,0),Lp(e)}function zp(){let e=new Date;return e.setSeconds(0,0),Lp(e)}async function Bp(){try{let e=await Mp();jp.value=e.jobs||e||[]}catch(e){console.error(`[jobs] fetch failed:`,e)}}function Vp(){let e=u(`recurring`),t=u(``),n=u(Rp()),r=u(``),i=u(O.value||``),a=u(``),o=u(``),s=u(!1),c=u(!1);p(()=>{Z.value===`jobs`&&Bp()},[Z.value]),p(()=>{i.value=O.value||``},[O.value]);let l=Ip(t.value),d=e.value===`once`?!!n.value:!!t.value.trim(),m=!!i.value&&d&&!!r.value.trim(),h=async()=>{if(m){a.value=``,o.value=``,s.value=!0;try{let l;if(e.value===`once`){let e=new Date(n.value);if(isNaN(e.getTime())){a.value=`Invalid date/time. Please select a valid date.`,s.value=!1;return}l={type:`once`,run_at:e.toISOString()}}else l={type:`recurring`,cron:t.value.trim()};await Np({agent_id:i.value,schedule:l,prompt:r.value.trim()}),t.value=``,r.value=``,c.value=!1,n.value=Rp(),o.value=e.value===`once`?`Job scheduled (one-time).`:`Recurring job created.`,setTimeout(()=>{o.value=``},4e3),await Bp()}catch(e){let t=e.error?.message||e.message||``;a.value=t||`Failed to create job. Check that all fields are filled and the schedule is valid.`}finally{s.value=!1}}},g=async e=>{try{await Pp(e),await Bp()}catch(e){a.value=e.error?.message||e.message||`Failed to cancel job`}};return f`
        <div>
            <div class="jobs-form">
                <select class="jobs-select" value=${i.value}
                        onChange=${e=>{i.value=e.target.value}}>
                    ${k.value.map(e=>f`
                        <option value=${e.id}>${e.name}</option>
                    `)}
                </select>

                <div class="schedule-mode-toggle">
                    <button class="cron-btn ${e.value===`recurring`?`active`:``}"
                            onClick=${()=>{e.value=`recurring`}}>
                        Recurring
                    </button>
                    <button class="cron-btn ${e.value===`once`?`active`:``}"
                            onClick=${()=>{e.value=`once`}}>
                        Run once
                    </button>
                </div>

                ${e.value===`recurring`?f`
                    <div class="cron-presets">
                        ${Fp.map(e=>f`
                            <button class="cron-btn ${t.value===e.cron?`active`:``}"
                                    title=${e.desc}
                                    onClick=${()=>{t.value=e.cron,c.value=!1}}>
                                ${e.label}
                            </button>
                        `)}
                        <button class="cron-btn ${c.value?`active`:``}"
                                title="Custom cron expression"
                                onClick=${()=>{c.value=!0,t.value=``}}>
                            custom
                        </button>
                    </div>

                    ${c.value&&f`
                        <input class="jobs-input" type="text" placeholder="min hour dom mon dow"
                               value=${t.value}
                               onInput=${e=>{t.value=e.target.value}} />
                    `}

                    ${t.value&&f`
                        <div class="cron-preview">${l}</div>
                    `}
                `:f`
                    <input class="jobs-input" type="datetime-local"
                           value=${n.value}
                           min=${zp()}
                           onInput=${e=>{n.value=e.target.value}} />
                `}

                <textarea class="jobs-textarea" rows="2" placeholder="Prompt for the agent..."
                          value=${r.value}
                          onInput=${e=>{r.value=e.target.value}}></textarea>

                ${!i.value&&f`
                    <div class="empty-state">
                        No agents available. Create an agent first.
                    </div>
                `}

                <button class="jobs-submit" onClick=${h}
                        disabled=${s.value||!m}>
                    ${s.value?`Scheduling...`:`Schedule`}
                </button>
            </div>

            ${o.value&&f`<div class="jobs-success">${o.value}</div>`}
            ${a.value&&f`<div class="jobs-error">${a.value}</div>`}

            <div class="jobs-divider"></div>

            ${jp.value.length===0?f`<div class="jobs-empty">No scheduled jobs</div>`:jp.value.map(e=>f`
                    <div class="job-item">
                        <div class="job-prompt">${e.prompt||e.task||`(no prompt)`}</div>
                        <div class="job-meta">
                            <span>${Ip(e.schedule?.cron)||(e.schedule?.type===`once`?`Once at `+rd(e.schedule.run_at):JSON.stringify(e.schedule))}</span>
                            ${e.next_run_at&&f`<span> | next: ${rd(e.next_run_at)}</span>`}
                            ${e.last_run_at&&f`<span> | last run: ${rd(e.last_run_at)}</span>`}
                        </div>
                        <span class="job-status-${e.status||`active`}">${e.status||`active`}</span>
                        ${e.status!==`cancelled`&&f`
                            <button class="job-cancel" onClick=${()=>g(e.id)}>Cancel</button>
                        `}
                    </div>
                `)}
        </div>
    `}var Hp=(e,t=50)=>v(`/audit?session_id=${e}&limit=${t}`),Up=50;async function Wp(e){if(!x.value){Os.value=null;return}try{let t=await Hp(x.value,e);Os.value=t.events||t||[]}catch{Os.value=[]}}function Gp(){let e=u(Up),t=u(!1);p(()=>{Z.value===`audit`&&(e.value=Up,Wp(Up))},[Z.value,x.value]);let n=async()=>{t.value=!0;try{let t=e.value+Up;e.value=t,await Wp(t)}catch(e){console.error(`[AuditTab] loadMore failed:`,e)}finally{t.value=!1}};if(!x.value)return f`<div class="empty-state">No session selected</div>`;if(Os.value===null)return f`<div class="loading-state">Loading...</div>`;if(Os.value.length===0)return f`<div class="empty-state">No audit events</div>`;let r=Os.value.length>=e.value;return f`
        <div>
            ${Os.value.map((e,t)=>f`
                <div class="audit-event" key=${e.id||`audit-${e.timestamp||``}-${t}`}>
                    <span class="audit-tool">${e.tool||e.action||`unknown`}</span>
                    <span class="${e.decision===`deny`?`audit-deny`:e.decision===`error`?`audit-error`:`audit-allow`}">
                        ${e.decision===`deny`?`denied`:e.decision===`error`?`error`:`allowed`}
                    </span>
                    ${e.timestamp&&f`<span class="audit-time">${rd(e.timestamp)}</span>`}
                    ${e.params&&f`
                        <div class="audit-params">${JSON.stringify(e.params).slice(0,120)}</div>
                    `}
                </div>
            `)}
            ${r&&f`
                <button class="audit-load-more"
                        onClick=${n}
                        disabled=${t.value}>
                    ${t.value?`Loading...`:`Load more`}
                </button>
            `}
        </div>
    `}var Kp=50,qp={completed:`✓`,failed:`✗`,cancelled:`⊘`,running:`⋯`},Jp={user:`user`,scheduled:`scheduled`,subagent:`subagent`,dm:`dm`,notification:`notif`,telegram:`telegram`},Yp={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`};function Xp(e){if(e==null)return`--`;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m`+(n>0?n+`s`:``)}function Zp(e){return e==null?`--`:e>=1e4?(e/1e3).toFixed(0)+`k`:e>=1e3?(e/1e3).toFixed(1)+`k`:String(e)}function Qp(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function $p(){let e=u([]),t=u(!1),n=u(``),r=async()=>{if(!O.value){e.value=[];return}t.value=!0,n.value=``;try{let t=await Ll(O.value,Kp);e.value=t.runs||[]}catch(t){console.error(`[RunsTab] fetch failed:`,t),n.value=t.error?.message||t.message||`Failed to load runs`,e.value=[]}finally{t.value=!1}};return p(()=>{Z.value===`runs`&&r()},[Z.value,O.value,ye.value]),O.value?t.value&&e.value.length===0?f`<div class="loading-state">Loading runs...</div>`:n.value?f`
            <div>
                <div class="runs-tab-error">${n.value}</div>
                <button class="runs-tab-retry" onClick=${r}>Retry</button>
            </div>
        `:e.value.length===0?f`<div class="runs-tab-empty">No runs yet</div>`:f`
        <div class="runs-tab">
            <div class="runs-tab-header">
                <span class="runs-tab-count">${e.value.length} run${e.value.length===1?``:`s`}</span>
                <button class="runs-tab-refresh" onClick=${r}
                        disabled=${t.value} title="Refresh">
                    ${t.value?`...`:`↻`}
                </button>
            </div>
            <div class="runs-tab-list">
                ${e.value.map(e=>f`
                    <div class="runs-tab-row runs-tab-row--${e.status||`unknown`}"
                         key=${e.run_id}
                         onClick=${()=>e.session_id&&Lu(e.session_id)}
                         title=${`Run `+e.run_id.slice(0,8)+` | Session `+(e.session_id||``).slice(0,8)}>
                        <div class="runs-tab-row-top">
                            <span class="runs-tab-status">${qp[e.status]||`·`}</span>
                            <span class="runs-tab-trigger runs-tab-trigger--${e.trigger||`user`}">
                                ${Jp[e.trigger]||e.trigger||`user`}
                            </span>
                            <span class="runs-tab-session-type">
                                ${Yp[e.session_type]||e.session_type||``}
                            </span>
                            <span class="runs-tab-time">${Qp(e.ts)}</span>
                        </div>
                        <div class="runs-tab-row-bottom">
                            <span class="runs-tab-duration">${Xp(e.duration_ms)}</span>
                            <span class="runs-tab-tools">${e.tool_call_count==null?``:e.tool_call_count+` tools`}</span>
                            <span class="runs-tab-tokens">
                                ${e.usage?Zp(e.usage.prompt_tokens)+` in / `+Zp(e.usage.completion_tokens)+` out`+(typeof e.usage.reasoning_tokens==`number`&&e.usage.reasoning_tokens>0?` (+`+Zp(e.usage.reasoning_tokens)+` reasoning)`:``)+(typeof e.usage.cache_read_input_tokens==`number`&&e.usage.cache_read_input_tokens>0?` (`+Zp(e.usage.cache_read_input_tokens)+` cached)`:``):``}
                            </span>
                        </div>
                    </div>
                `)}
            </div>
        </div>
    `:f`<div class="runs-tab-empty">No agent selected</div>`}function em(e,t=50,n=null){let r=`/agents/${e}/timeline?limit=${t}`;return n&&(r+=`&before=${encodeURIComponent(n)}`),v(r)}var tm=50,nm={run_started:`▶`,run_completed:`✓`,run_failed:`✗`,run_cancelled:`⊘`,run_ended:`■`,tool_call:`⚙`,message_received:`●`,message_sent:`○`,marker:`⚑`},rm={run_started:`started`,run_completed:`completed`,run_failed:`failed`,run_cancelled:`cancelled`,run_ended:`ended`,tool_call:`tool`,message_received:`message`,message_sent:`sent`,marker:`marker`},im={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`,episodic:`epis`};function am(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function om(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function sm(e){if(!e)return``;let t=new Date(e),n=new Date,r=new Date;return r.setDate(r.getDate()-1),t.toDateString()===n.toDateString()?`Today`:t.toDateString()===r.toDateString()?`Yesterday`:t.toLocaleDateString([],{weekday:`short`,month:`short`,day:`numeric`})}async function cm(e){if(!e||e===x.value)return;let t=we();gl(),c(()=>{x.value=e,M.value=null,ve.value=null,be([]),w.value=[],Os.value=null,xc(),tc.value=null,As.value=!0}),cu(O.value,e);try{await Jl(e,{isStale:()=>t!==Ce,logPrefix:`timelineTab`})}finally{t===Ce&&(As.value=!1)}}function lm(){let e=u([]),t=u(!1),n=u(!1),r=u(``),i=u(!1),a=u(null),o=async(o=!1)=>{if(!O.value){e.value=[];return}o?n.value=!0:t.value=!0,r.value=``;try{let t=o?a.value:null,n=await em(O.value,tm,t),r=n.events||[];o?e.value=[...e.value,...r]:e.value=r,i.value=n.pagination?.has_more||!1,a.value=n.pagination?.next_before||null}catch(t){console.error(`[TimelineTab] fetch failed:`,t),r.value=t.error?.message||t.message||`Failed to load timeline`,o||(e.value=[])}finally{t.value=!1,n.value=!1}};if(p(()=>{Z.value===`timeline`&&o(!1)},[Z.value,O.value]),!O.value)return f`<div class="tl-empty">No agent selected</div>`;if(t.value&&e.value.length===0)return f`<div class="loading-state">Loading timeline...</div>`;if(r.value)return f`
            <div>
                <div class="tl-error">${r.value}</div>
                <button class="tl-retry" onClick=${()=>o(!1)}>Retry</button>
            </div>
        `;if(e.value.length===0)return f`<div class="tl-empty">No activity yet</div>`;let s=new Set;{let t=``;for(let n of e.value){let e=sm(n.timestamp);e!==t&&(s.add(n),t=e)}}return f`
        <div class="tl-tab">
            <div class="tl-header">
                <span class="tl-count">${e.value.length} event${e.value.length===1?``:`s`}</span>
                <button class="tl-refresh" onClick=${()=>o(!1)}
                        disabled=${t.value} title="Refresh">
                    ${t.value?`...`:`↻`}
                </button>
            </div>
            <div class="tl-list">
                ${e.value.map((e,t)=>{let n=sm(e.timestamp),r=s.has(e),i=e.event_type===`tool_call`,a=e.event_type===`run_started`||e.event_type===`run_completed`||e.event_type===`run_failed`||e.event_type===`run_cancelled`||e.event_type===`run_ended`,o=e.metadata?.tool_name,c=e.event_type+`-`+e.timestamp+`-`+(e.run_id||``)+`-`+t+(o?`-`+o:``);return f`
                        ${r&&f`
                            <div class="tl-date-group" key=${`g-`+n}>${n}</div>
                        `}
                        <div class="tl-event tl-event--${e.event_type}${i?` tl-event--indent`:``}${a?` tl-event--run`:``}"
                             key=${c}
                             onClick=${()=>cm(e.session_id)}
                             title=${`Session `+(e.session_id||``).slice(0,8)+(e.run_id?` | Run `+e.run_id.slice(0,8):``)}>
                            <span class="tl-time">${om(e.timestamp)}</span>
                            <span class="tl-icon tl-icon--${e.event_type}">${nm[e.event_type]||`·`}</span>
                            <span class="tl-session-badge tl-session-badge--${e.session_type||`chat`}">
                                ${im[e.session_type]||e.session_type||`chat`}
                            </span>
                            <span class="tl-event-label">${rm[e.event_type]||e.event_type}</span>
                            <span class="tl-ago">${am(e.timestamp)}</span>
                        </div>
                        ${e.summary&&f`
                            <div class="tl-summary${i?` tl-summary--indent`:``}"
                                 onClick=${()=>cm(e.session_id)}>
                                ${e.summary}
                            </div>
                        `}
                    `})}
            </div>
            ${i.value&&f`
                <button class="tl-load-more"
                        onClick=${()=>o(!0)}
                        disabled=${n.value}>
                    ${n.value?`Loading...`:`Load more`}
                </button>
            `}
        </div>
    `}function um({tab:e}){return e===`agents`?f`<${Sp} />`:e===`workspace`?f`<${Ap} />`:e===`runs`?f`<${$p} />`:e===`jobs`?f`<${Vp} />`:e===`audit`?f`<${Gp} />`:e===`timeline`?f`<${lm} />`:null}function dm(){hu.value=null}function fm(){return hu.value?f`
        <div id="panel" class="open">
            <div class="panel-header">
                <span class="panel-header-title">${Z.value.charAt(0).toUpperCase()+Z.value.slice(1)}</span>
                <button class="panel-close-btn" title="Close panel" aria-label="Close panel"
                        onClick=${dm}>\u00D7</button>
            </div>
            <div class="panel-body">
                <${um} tab=${Z.value} />
            </div>
        </div>
    `:null}var pm=()=>v(`/auth/keys`),mm=(e,t)=>oe(`/auth/keys`,{provider:e,key:t}),hm=e=>g(`/auth/keys/${e}`),gm=[`openai`,`anthropic`,`openrouter`,`gemini`];function _m({title:e,defaultOpen:t=!1,children:n}){let r=u(t);return f`
        <div class="settings-section">
            <button type="button" class="settings-section-toggle"
                    aria-expanded=${r.value}
                    onClick=${e=>{e.stopPropagation(),r.value=!r.value}}>
                <span class="settings-section-arrow ${r.value?`open`:``}">▶</span>
                <span class="settings-section-title">${e}</span>
            </button>
            <div class="settings-section-body ${r.value?`open`:``}"
                 aria-hidden=${!r.value}>
                ${n}
            </div>
        </div>
    `}function vm({label:e,value:t,desc:n}){return f`
        <div class="settings-info-row">
            <div class="settings-info-row-header">
                <span class="settings-info-row-label">${e}</span>
                <span class="settings-info-row-value">${t}</span>
            </div>
            ${n&&f`<span class="settings-hint">${n}</span>`}
        </div>
    `}function $({label:e,desc:t,children:n}){return f`
        <div class="settings-info-row">
            <div class="settings-info-row-header" style="flex-wrap:wrap;gap:6px;">
                <span class="settings-info-row-label">${e}</span>
                ${n}
            </div>
            ${t&&f`<span class="settings-hint">${t}</span>`}
        </div>
    `}function ym(){let e=u([]),t=u(null),n=u(``),r=u(!1),i=u(``),a=async()=>{try{let t=await pm();e.value=t.keys||[]}catch(e){console.error(`[auth] list keys failed:`,e)}};p(()=>{a()},[]);let o=async e=>{if(n.value.trim()){r.value=!0,i.value=``;try{await mm(e,n.value.trim()),n.value=``,t.value=null,await a()}catch(e){i.value=e.error?.message||e.message||`Failed to save key`}finally{r.value=!1}}},s=async e=>{try{await hm(e),await a()}catch(e){i.value=e.error?.message||e.message||`Failed to remove key`}};return f`
        <div class="settings-row">
            <label class="settings-label">API Keys</label>
            ${gm.map(i=>{let a=e.value.find(e=>e.provider===i),c=a?.configured,l=a?.source||`none`,u=a?.key||``;return t.value===i?f`
                        <div class="api-key-row" key=${i}>
                            <span class="api-key-provider">${i}</span>
                            <input class="settings-input" type="password"
                                   autocomplete="off"
                                   placeholder="Paste API key..."
                                   value=${n.value}
                                   onInput=${e=>{n.value=e.target.value}}
                                   onKeyDown=${e=>{e.key===`Enter`&&o(i)}} />
                            <div class="api-key-actions">
                                <button class="api-key-btn save" onClick=${()=>o(i)}
                                        disabled=${r.value}>
                                    ${r.value?`...`:`Save`}
                                </button>
                                <button class="api-key-btn" onClick=${()=>{t.value=null,n.value=``}}>
                                    Cancel
                                </button>
                            </div>
                        </div>
                    `:f`
                    <div class="api-key-row" key=${i}>
                        <span class="api-key-provider">${i}</span>
                        <span class="api-key-value ${c?`set`:`unset`}">
                            ${c?u:`not configured`}
                        </span>
                        ${c&&l===`secrets`&&f`
                            <span class="api-key-source">stored</span>
                        `}
                        <div class="api-key-actions">
                            <button class="api-key-btn" onClick=${()=>{t.value=i,n.value=``}}>
                                ${c?`Change`:`Set`}
                            </button>
                            ${c&&l===`secrets`&&f`
                                <button class="api-key-btn remove" onClick=${()=>s(i)}>Remove</button>
                            `}
                        </div>
                    </div>
                `})}
            ${i.value&&f`<div class="inline-error">${i.value}</div>`}
        </div>
    `}function bm({open:e,onClose:t}){let n=u(!1),r=u(!1),i=u(``),a=u(``),o=u(``),s=u(``),c=u(``),l=u(``),d=u(``),m=u(``),h=u(``),g=u(!0),_=u(``),v=u(``),y=u(``),b=u(``),x=u(``),ee=u(``),S=u(``),te=u(``),ne=u(!0),re=u(!1),C=u(``),ie=u(``),ae=u(!0),oe=u(!1),se=u(``),ce=u(!1),le=u(!1),ue=u(!1),de=u(``);if(p(()=>{if(e){let e=Ts.value,t=e.context||{},u=e.session||{},f=e.tools||{},p=e.llm||{},ue=p.anthropic||{},w=p.openai||{},T=p.gemini||{};i.value=t.strategy||`truncate`,a.value=t.max_input_tokens==null?``:String(t.max_input_tokens),o.value=t.compact_trigger_pct==null?``:String(t.compact_trigger_pct),s.value=t.compact_retain_pct==null?``:String(t.compact_retain_pct),c.value=t.summary_model||``,l.value=t.summary_provider||``,d.value=u.max_messages==null?``:String(u.max_messages),m.value=u.max_context_tokens==null?``:String(u.max_context_tokens),h.value=u.idle_timeout_secs==null?``:String(u.idle_timeout_secs),g.value=u.auto_archive==null||u.auto_archive,_.value=u.archive_ttl_secs==null?``:String(u.archive_ttl_secs),v.value=f.shell_policy||`sandboxed`,y.value=f.sandbox_root||`.`,b.value=f.timeout_secs==null?``:String(f.timeout_secs),x.value=f.max_output_bytes==null?``:String(f.max_output_bytes),ee.value=e.model||``,S.value=e.provider||``,te.value=ue.thinking_budget_tokens==null?``:String(ue.thinking_budget_tokens),ne.value=ue.prompt_cache_enabled==null||!!ue.prompt_cache_enabled,re.value=!1,C.value=w.reasoning_effort||``,ie.value=T.thinking_budget==null?``:String(T.thinking_budget),ae.value=T.cache_enabled==null||!!T.cache_enabled,oe.value=!1,se.value=T.cache_ttl_seconds==null?``:String(T.cache_ttl_seconds);let fe=k.value.find(e=>e.id===O.value);ce.value=!!(fe&&fe.debug_mode),le.value=!1,n.value=!1,r.value=!1,de.value=``}},[e]),!e)return null;let w=Ts.value,T=w.context||{},fe=w.session||{},pe=w.logging||{},me=w.tools||{},E=w.llm||{},he=E.anthropic||{},ge=E.openai||{},D=E.gemini||{},A=async()=>{ue.value=!0,de.value=``,n.value=!1;let e={},u={};i.value&&i.value!==(T.strategy||``)&&(u.strategy=i.value);let f=parseInt(a.value,10);!isNaN(f)&&f!==T.max_input_tokens&&(u.max_input_tokens=f);let p=parseFloat(o.value);!isNaN(p)&&p!==T.compact_trigger_pct&&(u.compact_trigger_pct=p);let pe=parseFloat(s.value);!isNaN(pe)&&pe!==T.compact_retain_pct&&(u.compact_retain_pct=pe),c.value!==(T.summary_model||``)&&(u.summary_model=c.value),l.value!==(T.summary_provider||``)&&(u.summary_provider=l.value),Object.keys(u).length>0&&(e.context=u);let E={},A=parseInt(d.value,10);!isNaN(A)&&A!==fe.max_messages&&(E.max_messages=A);let j=parseInt(m.value,10);!isNaN(j)&&j!==fe.max_context_tokens&&(E.max_context_tokens=j);let _e=parseInt(h.value,10);!isNaN(_e)&&_e!==fe.idle_timeout_secs&&(E.idle_timeout_secs=_e),g.value!==fe.auto_archive&&(E.auto_archive=g.value);let ve=parseInt(_.value,10);!isNaN(ve)&&ve!==fe.archive_ttl_secs&&(E.archive_ttl_secs=ve),Object.keys(E).length>0&&(e.session=E);let ye={};v.value&&v.value!==(me.shell_policy||``)&&(ye.shell_policy=v.value),y.value!==(me.sandbox_root||``)&&(ye.sandbox_root=y.value);let M=parseInt(b.value,10);!isNaN(M)&&M!==me.timeout_secs&&(ye.timeout_secs=M);let N=parseInt(x.value,10);!isNaN(N)&&N!==me.max_output_bytes&&(ye.max_output_bytes=N),Object.keys(ye).length>0&&(e.tools=ye);let P={},be={},xe=parseInt(te.value,10);te.value!==``&&!isNaN(xe)&&xe!==he.thinking_budget_tokens&&(be.thinking_budget_tokens=xe),re.value&&ne.value!==!!he.prompt_cache_enabled&&(be.prompt_cache_enabled=ne.value),Object.keys(be).length>0&&(P.anthropic=be);let Se={},F=ge.reasoning_effort||``;C.value!==F&&(Se.reasoning_effort=C.value),Object.keys(Se).length>0&&(P.openai=Se);let I={},Ce=parseInt(ie.value,10);ie.value!==``&&!isNaN(Ce)&&Ce!==D.thinking_budget&&(I.thinking_budget=Ce),oe.value&&ae.value!==!!D.cache_enabled&&(I.cache_enabled=ae.value);let we=parseInt(se.value,10);se.value!==``&&!isNaN(we)&&we!==D.cache_ttl_seconds&&(I.cache_ttl_seconds=we),Object.keys(I).length>0&&(P.gemini=I),Object.keys(P).length>0&&(e.llm=P),ee.value&&ee.value!==(w.model||``)&&(e.model=ee.value),S.value&&S.value!==(w.provider||``)&&(e.provider=S.value);let Te=!1;if(Object.keys(e).length>0)try{let t=await gs(e);t&&t.restart_required&&(Te=!0),await Es()}catch(e){let t=Array.isArray(e.errors)?e.errors.join(`; `):null;de.value=t||e.message||`Failed to save server settings`}if(le.value&&O.value){let e=k.value.find(e=>e.id===O.value),t=pp(e&&e.debug_mode,ce.value);if(Object.keys(t).length>0)try{await qf(O.value,t);let e=await Gf();e&&Array.isArray(e.agents)&&(k.value=e.agents)}catch(e){de.value=e.error?.message||e.message||`Failed to save debug mode`}}de.value||(n.value=!0,r.value=Te),ue.value=!1,!de.value&&!Te&&setTimeout(()=>t(),600)},j=e=>{e.target===e.currentTarget&&t()},_e=me.enabled||w.enabled_tools||[];return f`
        <div class="settings-overlay open" onClick=${j}>
            <div class="settings-modal">
                <h2>Settings</h2>

                <!-- Security: API Keys -->
                <${ym} />

                <div class="settings-divider"></div>

                <!-- Per-run config overrides were removed in the #941
                     pivot. Per-agent values (model / provider / posture /
                     reasoning budgets) live on the agent record and are
                     edited from the Agents panel; server defaults are
                     edited below and propagate to the next run via
                     PATCH /settings. -->

                <!-- Debug (per-agent, #1003) — context-window inspection
                     toggle for the currently-active agent. PATCHes
                     /agents/{active}, not /settings. The full per-agent
                     config surface lives in the Agents panel; this row
                     is mirrored here as a discoverable shortcut for
                     the most common Debug-mode flow. -->
                <${_m} key="debug" title="Debug" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Per-agent context-window inspection. When enabled, every turn from the active agent
                        emits a snapshot of the full assembled LLM context (system prompts, workspace,
                        episodic memory, history, tool definitions). Works for both webchat and DM sessions —
                        for DMs, each turn shows the per-perspective context the agent currently being
                        inspected sees on its turn. Takes effect on the next run; previous turns are not
                        retroactively shown.
                    </span>
                    <${$} label="Debug mode (active agent)"
                        desc="Mirrors the per-agent toggle in the Agents panel. Applies only to the currently-selected agent.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${ce.value}
                                   disabled=${!O.value}
                                   onChange=${e=>{ce.value=e.target.checked,le.value=!0}} />
                            <span>${ce.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                <//>

                <!-- Default LLM (server-level, editable, restart-required).
                     Pre-PR-941 this row was a disabled-display of the per-run
                     model picker; PR-941 removed that path entirely, which
                     left no UI surface for the actual server-default model.
                     The section is restored here as a persistence-only knob
                     (the run path reads by-value clones of the LlmClient
                     that are not behind a shared lock, so a hot-swap would
                     need a bigger refactor — out of scope for this
                     restoration). Backend persists into settings.json and
                     re-applies on the next daemon start; on PATCH the
                     response carries restart_required:true so the operator
                     gets a yellow banner instead of a 600ms Saved! flash. -->
                <${_m} key="defaults" title="Default LLM (model / provider)" defaultOpen=${!0}>
                    <span class="settings-hint settings-section-desc">
                        Server-default LLM identity — new agents inherit these values when they don't
                        carry a per-agent override (per-agent values live on the agent record and are
                        edited from the Agents panel). Changes here are persisted to
                        <code>settings.json</code> and take effect on the next daemon restart; in-flight
                        runs continue to use the boot-time snapshot, mirroring the Logging section.
                    </span>
                    <${$} label="Default LLM model"
                        desc="Model id sent to the resolved provider's wire (e.g. z-ai/glm-5.2, claude-sonnet-4-6, gpt-5.4). Pick from the suggestions list or type any model the provider accepts.">
                        <input class="settings-input settings-input-sm" type="text"
                               list="model-suggestions"
                               placeholder=${w.model||`model id`}
                               value=${ee.value}
                               onInput=${e=>{ee.value=e.target.value}} />
                        <span class="settings-effective">
                            <${rp} value=${ee.value.trim()} defaultValue=${w.model} />
                        </span>
                    <//>
                    <${$} label="Default LLM provider"
                        desc="Provider whose [llm.providers.NAME] entry the resolved model is sent to. Must be configured under [llm.providers] in alms.toml with a resolvable API key.">
                        <select class="settings-select settings-input-sm"
                                value=${S.value}
                                onChange=${e=>{S.value=e.target.value}}>
                            ${(w.llm_providers&&w.llm_providers.length>0?w.llm_providers:gm).map(e=>{let t=tp(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                    <datalist id="model-suggestions">
                        ${Zf.map(e=>f`<option value=${e} key=${e}></option>`)}
                    </datalist>
                <//>

                <!-- Context (server-level, editable) -->
                <${_m} key="ctx" title="Context" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        truncate fits the most recent history into the token budget.
                        compact summarises older messages once the session crosses the trigger threshold.
                        Changes apply to the next run.
                    </span>
                    <${$} label="Strategy"
                        desc="truncate = drop oldest messages to fit the budget. compact = summarise old + keep recent verbatim once history crosses the trigger threshold.">
                        <select class="settings-select settings-input-sm"
                                value=${i.value}
                                onChange=${e=>{i.value=e.target.value}}>
                            <option value="truncate">truncate — drop oldest messages to fit budget</option>
                            <option value="compact">compact — summarise old + keep recent verbatim</option>
                        </select>
                    <//>
                    <${$} label="Max input tokens"
                        desc="Token budget per LLM request (should match your model's context window).">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${a.value}
                               onInput=${e=>{a.value=e.target.value}} />
                    <//>
                    ${i.value===`compact`?f`
                    <${$} label="Compact trigger %"
                        desc="Compact strategy: trigger compaction when assembled history exceeds this fraction of the effective history budget (max_input_tokens minus system / input / episodic / reserve overhead). Range: 0.50–0.95.">
                        <input class="settings-input settings-input-sm" type="number"
                               min="0.50" max="0.95" step="0.05"
                               value=${o.value}
                               onInput=${e=>{o.value=e.target.value}} />
                    <//>
                    <${$} label="Compact retain %"
                        desc="Compact strategy: retain at most this fraction of the effective history budget (max_input_tokens minus system / input / episodic / reserve overhead) worth of recent verbatim messages after compaction. Range: 0.20–0.60.">
                        <input class="settings-input settings-input-sm" type="number"
                               min="0.20" max="0.60" step="0.05"
                               value=${s.value}
                               onInput=${e=>{s.value=e.target.value}} />
                    <//>
                    `:null}
                <//>

                <!-- Summary (server-level, editable) — controls BOTH the
                     in-loop compact-strategy compaction AND the post-run
                     episodic memory generation. Lifted out of the Context
                     section to make the dual-path scope obvious. -->
                <${_m} key="summary" title="Summary (compact strategy + episodic memory)" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Optional dedicated provider/model for the summary task. Drives both the in-loop compact-strategy compaction
                        (rolling context window) and the per-run episodic memory generation. Both fields must be set together — partial
                        configurations are rejected so the user-supplied summary_model is never silently paired with the agent's primary provider.
                        Per-agent overrides live on the agent record (Agents panel).
                    </span>
                    <${$} label="Summary model"
                        desc="Cheaper model for generating summaries. Set together with Summary provider, or leave both empty to use the agent's main LLM.">
                        <input class="settings-input settings-input-sm" type="text"
                               placeholder="leave empty to use the agent's main LLM"
                               list="model-suggestions"
                               value=${c.value}
                               onInput=${e=>{c.value=e.target.value}} />
                        <span class="settings-effective">
                            <${rp} value=${c.value.trim()} defaultValue=${w.model} />
                        </span>
                    <//>
                    <${$} label="Summary provider"
                        desc="Dedicated provider for the summary task. Must be configured under [llm.providers.<name>] with a resolvable API key. Set together with Summary model.">
                        <select class="settings-select settings-input-sm"
                                value=${l.value}
                                onChange=${e=>{l.value=e.target.value}}>
                            <option value="">Unset (no dedicated summary task)</option>
                            ${(w.llm_providers&&w.llm_providers.length>0?w.llm_providers:gm).map(e=>{let t=tp(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                <//>

                <!-- Session (server-level, editable) -->
                <${_m} key="sess" title="Session" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Controls session storage and retention. Changes apply to the next run.
                    </span>
                    <${$} label="Max messages"
                        desc="Maximum messages stored per session.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${d.value}
                               onInput=${e=>{d.value=e.target.value}} />
                    <//>
                    <${$} label="Max context tokens"
                        desc="Maximum tokens retained in session history (must be >= context max_input_tokens).">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${m.value}
                               onInput=${e=>{m.value=e.target.value}} />
                    <//>
                    <${$} label="Idle timeout (seconds)"
                        desc="Time before a session is considered idle.">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${h.value}
                               onInput=${e=>{h.value=e.target.value}} />
                    <//>
                    <${$} label="Auto archive"
                        desc="Automatically archive idle sessions.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${g.value}
                                   onChange=${e=>{g.value=e.target.checked}} />
                            <span>${g.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                    <${$} label="Archive TTL (seconds)"
                        desc="Delete archived sessions after this duration.">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${_.value}
                               onInput=${e=>{_.value=e.target.value}} />
                    <//>
                <//>

                <!-- Tools (server-level, editable) -->
                <${_m} key="tools" title="Tools" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Tool execution settings. Changes apply to the next run.
                    </span>
                    <${$} label="Shell policy"
                        desc="sandboxed = restrict shell cwd to sandbox root. unrestricted = no cwd restriction.">
                        <select class="settings-select settings-input-sm"
                                value=${v.value}
                                onChange=${e=>{v.value=e.target.value}}>
                            <option value="sandboxed">sandboxed</option>
                            <option value="unrestricted">unrestricted</option>
                        </select>
                    <//>
                    <${$} label="Sandbox root"
                        desc="Filesystem sandbox root for fs_* tools. Empty = unrestricted.">
                        <input class="settings-input settings-input-sm" type="text"
                               value=${y.value}
                               onInput=${e=>{y.value=e.target.value}} />
                    <//>
                    <${$} label="Tool timeout (seconds)"
                        desc="Maximum execution time per tool call.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${b.value}
                               onInput=${e=>{b.value=e.target.value}} />
                    <//>
                    <${$} label="Max output (bytes)"
                        desc="Maximum bytes returned from a single tool call.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${x.value}
                               onInput=${e=>{x.value=e.target.value}} />
                    <//>
                    <${vm} label="Enabled tools" value=${`${_e.length} tools`}
                        desc=${_e.join(`, `)} />
                <//>

                <!-- LLM Providers (server-level, editable) — #809 / #804 Slice A -->
                <${_m} key="llm" title="LLM Providers" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Server-level reasoning &amp; caching defaults. Mutations propagate to the next HTTP-triggered run without restart; Telegram-triggered runs use a boot-time snapshot until the daemon is restarted.
                    </span>

                    <h4 class="settings-llm-subhead">Anthropic</h4>
                    <${$} label="Thinking budget tokens"
                        desc="0 = extended thinking off. Leave blank to keep the current server value. The wire surface has no clear sentinel — once PATCHed, revert by editing settings.json + restart.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${he.thinking_budget_tokens==null?`unset`:String(he.thinking_budget_tokens)}
                               value=${te.value}
                               onInput=${e=>{te.value=e.target.value}} />
                    <//>
                    <${$} label="Prompt cache enabled"
                        desc="Anthropic prefix caching (5-minute TTL). Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${ne.value}
                                   onChange=${e=>{ne.value=e.target.checked,re.value=!0}} />
                            <span>${ne.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>

                    <h4 class="settings-llm-subhead">OpenAI / OpenRouter</h4>
                    <${$} label="Reasoning effort"
                        desc="Applies to o-series, GPT-5, and reasoning-capable Grok models. Auto-stripped on non-reasoning models. Choose Unset to clear an existing override.">
                        <select class="settings-select settings-input-sm"
                                value=${C.value}
                                onChange=${e=>{C.value=e.target.value}}>
                            <option value="">Unset (no override)</option>
                            <option value="minimal">minimal</option>
                            <option value="low">low</option>
                            <option value="medium">medium</option>
                            <option value="high">high</option>
                        </select>
                    <//>

                    <h4 class="settings-llm-subhead">Gemini</h4>
                    <${$} label="Thinking budget"
                        desc="0 = extended thinking off. Leave blank to keep the current server value. Once PATCHed, this value can only be reverted by editing settings.json + restart.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${D.thinking_budget==null?`unset`:String(D.thinking_budget)}
                               value=${ie.value}
                               onInput=${e=>{ie.value=e.target.value}} />
                    <//>
                    <${$} label="Cache enabled"
                        desc="Gemini context caching via cachedContents. Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${ae.value}
                                   onChange=${e=>{ae.value=e.target.checked,oe.value=!0}} />
                            <span>${ae.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                    <${$} label="Cache TTL (seconds)"
                        desc="Lifetime of a Gemini cache entry. Must be > 0.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="60"
                               placeholder=${D.cache_ttl_seconds==null?`300`:String(D.cache_ttl_seconds)}
                               value=${se.value}
                               onInput=${e=>{se.value=e.target.value}} />
                    <//>
                <//>

                <!-- Logging (server-level, read-only) -->
                <${_m} key="log" title="Logging" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        File-based logging settings. Requires restart to change.
                    </span>
                    <${vm} label="File logging" value=${pe.file_enabled==null?`--`:pe.file_enabled?`enabled`:`disabled`}
                        desc="Whether persistent file logging is active." />
                    <${vm} label="File level" value=${pe.file_level||`--`}
                        desc="Log level for file output (trace, debug, info, warn, error)." />
                    <${vm} label="Rotation" value=${pe.rotation||`--`}
                        desc="Log rotation policy: daily, hourly, or never." />
                    <${vm} label="Log directory" value=${pe.log_dir||`default (data/logs/)`}
                        desc="Directory where log files are written." />
                <//>

                <div class="settings-divider"></div>

                <!-- Server info (compact) -->
                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Version: <span class="settings-info-value">${w.version||`unknown`}</span></div>
                        <div>Base URL: <span class="settings-info-value">${w.base_url||`unknown`}</span></div>
                        <div>Stream timeout: <span class="settings-info-value">${w.stream_chunk_timeout_secs||180}s</span></div>
                    </div>
                </div>

                ${de.value&&f`
                    <div class="settings-error">
                        Failed to save server settings: ${de.value}
                    </div>
                `}

                ${r.value&&f`
                    <div class="settings-hint" style="background:#3a2d10;border:1px solid #8a6b1a;color:#f0c264;padding:10px;border-radius:6px;margin:8px 0;">
                        <strong>Saved — restart required.</strong>
                        Server-default model / provider changes are persisted to
                        <code>settings.json</code> but won't reach in-flight runs
                        until the daemon restarts. New runs created after the
                        restart will pick up the new values.
                    </div>
                `}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${t}>Cancel</button>
                    <button class="settings-save" onClick=${A}
                            disabled=${ue.value}>
                        ${ue.value?`Saving...`:n.value?`Saved!`:`Apply`}
                    </button>
                </div>
            </div>
        </div>
    `}function xm(){let e=u(``),t=u(``),n=u(!1);return f`
        <div id="onboarding">
            <form class="onboard-card" onSubmit=${async r=>{r.preventDefault();let i=e.value.trim();if(i){if(!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(i)){t.value=`Invalid name: lowercase letters, digits, hyphens only (1-64 chars, no trailing hyphen)`;return}n.value=!0,t.value=``;try{let e=await Kf({name:i,is_default:!0});k.value=(await Gf()).agents||[];let t=e.id||(k.value.find(e=>e.name===i)||{}).id;t?await mu(t):console.warn(`[onboarding] POST /agents returned no id for agent:`,i,e)}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}}}}>
                <h2>Welcome to ALMS</h2>
                <p>Create your first agent to get started. The agent will introduce itself and learn about you in a short setup conversation.</p>
                <div>
                    <label>Agent name</label>
                    <input type="text" placeholder="my-agent" autofocus
                        value=${e.value}
                        onInput=${t=>{e.value=t.target.value}}
                        disabled=${n.value} />
                    <div class="onboard-hint">lowercase, digits, hyphens (1-64 chars)</div>
                </div>
                <button class="onboard-btn" type="submit" disabled=${n.value||!e.value.trim()}>
                    ${n.value?`Creating...`:`Create Agent`}
                </button>
                <div class="onboard-error">${t.value}</div>
            </form>
        </div>
    `}function Sm(e){if(!e)return``;if(e.status===`done`)return`Done`;if(e.status===`fail`)return`Failed`;if(e.status===`cancelled`)return`Cancelled`;let t=e.activity;if(!t||!t.kind)return`Starting…`;switch(t.kind){case`reasoning`:return`Reasoning…`;case`writing`:return`Writing…`;case`tool_start`:return t.tool?`Using ${t.tool}`:`Using tool`;case`tool_end`:return`Running…`;default:return`Running…`}}function Cm(){let e=Object.entries(q.value);return e.length===0?null:f`
        <div class="sa-bar" aria-label="Subagent status bar">
            ${e.map(([e,t])=>{let n=t.status===`running`,r=t.status===`done`?`✓`:`✗`,i=t.displayName||e,a=Sm(t),o=()=>{t.sessionId&&yc(t.sessionId)},s=e=>{fp(e)&&o()},c=e=>{dp(e)&&(e.preventDefault(),o())},l=t.task?`${i}: ${t.task} — open subagent session`:`${i} — open subagent session`,u=Vs(t.sessionId),d=e=>t=>{if(t.stopPropagation(),t.key===`Escape`){t.preventDefault(),Us();return}(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),e(t))},p=e=>{e.stopPropagation(),Hs(t.sessionId)},m=e=>{e.stopPropagation(),Gs(t.sessionId)},h=e=>{e.stopPropagation(),Us()};return f`
                    <div class="sa-chip ${n?`running`:t.status}"
                         role="button"
                         tabindex="0"
                         title=${l}
                         onClick=${s}
                         onKeyDown=${c}>
                        ${n?f`<span class="tc-spinner"></span>`:f`<span>${r}</span>`}
                        <span class="sa-chip-name">${i}</span>
                        ${a&&f`<span class="sa-chip-status">${a}</span>`}
                        ${Bs(t)&&(u?f`
                                <span class="sa-cancel-confirm-group" role="group"
                                      aria-label="Confirm cancel subagent">
                                    <span class="sa-cancel-confirm-label">Cancel?</span>
                                    <button class="sa-confirm-btn sa-confirm-yes"
                                            title="Yes, cancel this subagent"
                                            aria-label="Yes, cancel this subagent"
                                            onClick=${m}
                                            onKeyDown=${d(m)}>Yes</button>
                                    <button class="sa-confirm-btn sa-confirm-no"
                                            title="No, keep it running"
                                            aria-label="No, keep it running"
                                            onClick=${h}
                                            onKeyDown=${d(h)}>No</button>
                                </span>
                            `:f`
                                <button class="sa-chip-cancel"
                                        title="Cancel this subagent"
                                        aria-label="Cancel this subagent"
                                        onClick=${p}
                                        onKeyDown=${d(p)}>✕</button>
                            `)}
                    </div>
                `})}
        </div>
    `}function wm(){let e=A.value,{phase:t,detail:i}=Cc.value,a=wc(t,i),o=u(!1),s=u(!1),c=n(null),l=r(()=>{o.value=!o.value},[]),d=r(()=>{o.value=!1,s.value=!0},[]);return p(()=>{if(!o.value)return;let e=e=>{c.current&&!c.current.contains(e.target)&&(o.value=!1)};return document.addEventListener(`click`,e,!0),()=>document.removeEventListener(`click`,e,!0)},[o.value]),e?f`
        <div class="agent-header-bar">
            <div class="agent-header-bar-left">
                <span class="agent-header-bar-name">${e.name}</span>
                ${a&&f`
                    <span class="agent-status-label">${a}</span>
                `}
            </div>
            <div class="agent-header-bar-right">
                <button class="hbtn agent-bar-btn ${hu.value===`workspace`?`active`:``}"
                        title="Workspace files"
                        aria-label="Open workspace panel"
                        onClick=${()=>gu(`workspace`)}>
                    <${Su} />
                    <span class="agent-bar-btn-label">Workspace</span>
                </button>
                <button class="hbtn agent-bar-btn ${hu.value===`timeline`?`active`:``}"
                        title="Agent timeline"
                        aria-label="Open timeline panel"
                        onClick=${()=>gu(`timeline`)}>
                    <${xu} />
                    <span class="agent-bar-btn-label">Timeline</span>
                </button>
                <button class="hbtn agent-bar-btn ${hu.value===`runs`?`active`:``}"
                        title="Agent runs"
                        aria-label="Open runs panel"
                        onClick=${()=>gu(`runs`)}>
                    <${Eu} />
                    <span class="agent-bar-btn-label">Runs</span>
                </button>
                <div class="agent-menu-anchor" ref=${c}>
                    <button class="hbtn agent-bar-btn"
                            title="Agent menu"
                            aria-label="Open agent menu"
                            aria-expanded=${o.value}
                            onClick=${l}>
                        <span class="agent-menu-dots" aria-hidden="true">\u22EF</span>
                    </button>
                    ${o.value&&f`
                        <div class="agent-menu-dropdown">
                            <button class="agent-menu-item" onClick=${d}>
                                Settings
                            </button>
                        </div>
                    `}
                </div>
            </div>

            ${s.value&&f`
                <${bp}
                    agent=${e}
                    onClose=${()=>{s.value=!1}} />
            `}
        </div>
    `:null}var Tm=d(!1),Em=new Set;function Dm(e,t,n){return e.fromAgent?e.fromAgent===t[0]?`left`:`right`:e.type===`agent`||e.role===`assistant`?n?n===t[0]?`left`:`right`:`left`:e.type===`user`||e.role===`user`?n?n===t[0]?`right`:`left`:`right`:`center`}function Om({msg:e,participants:t,perspectiveAgent:r}){let i=Dm(e,t,r),a=e.fromAgent||(i===`left`?t[0]:t[1])||`?`,o=s(e.text||``),c=e.type===`agent`||e.role===`assistant`,l=n(null);return p(()=>{c&&bd(l.current)},[o,c]),f`
        <div class="dm-msg dm-msg-${i}">
            <div class="dm-msg-name-row dm-msg-name-row-${i}">
                <div class="dm-msg-name">${a}</div>
                <${xd} ts=${e.ts} />
            </div>
            <div class="dm-msg-bubble markdown-body" ref=${l}
                 dangerouslySetInnerHTML=${{__html:o}} />
        </div>
    `}function km({text:e}){return f`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">${e}</span>
        </div>
    `}function Am(e,t){if(!e)return!1;let n=e.trim();if(!n)return!1;for(let e of t||[]){if(e.tool!==`send_message`)continue;let t=e.params&&typeof e.params.message==`string`?e.params.message.trim():``;if(t&&t===n)return!0}return!1}function jm({runId:e,agentName:t,thinkingText:n,tools:r,status:i,isLive:a}){let[s,c]=o(!1),l=a&&qc.value.get(e)||``,u=n||l,d=Am(u,r)?``:u,p=(r||[]).filter(e=>!(e.tool===`send_message`&&e.status===`done`)),m=p.length,h=(r||[]).length>0;return!a&&!h&&(!d||!d.trim())?null:f`
        <div class=${`dm-reasoning-block`+(i===`failed`?` dm-reasoning-block--failed`:``)+(a?` dm-reasoning-block--live`:``)}>
            <div class="dm-reasoning-header" onClick=${()=>c(!s)}>
                <span class="dm-reasoning-toggle">${s?`▼`:`▶`}</span>
                <span class="dm-reasoning-summary">${t?`${t} reasoning -- ${m} tool call${m===1?``:`s`}`:`Agent reasoning -- ${m} tool call${m===1?``:`s`}`}</span>
                ${a&&f`<span class="dm-reasoning-spinner" />`}
            </div>
            ${s&&f`
                <div class="dm-reasoning-body">
                    ${d&&d.trim()&&f`
                        <pre class="dm-reasoning-thinking">${d}</pre>
                    `}
                    ${p.map(e=>f`
                        <${mf} key=${e.id} ...${e} />
                    `)}
                </div>
            `}
        </div>
    `}async function Mm(){let e=x.value;if(!(!e||Tm.value)){Tm.value=!0;try{await Cs(e)}catch(e){console.error(`[cancel-dm] failed:`,e)}finally{Tm.value=!1}}}function Nm(){let e=n(null),r=S.value;p(()=>{let n=0,r=t(()=>{F.value,cancelAnimationFrame(n),n=requestAnimationFrame(()=>{ad(e.current)})});return()=>{cancelAnimationFrame(n),r()}},[]);let i=F.value,a=A.value?A.value.name:null,o=r.length>=2?`${r[0]} <-> ${r[1]}`:`DM conversation`,s=!!M.value,c=!!X.value,l=s||c,u=Tm.value;return f`
        <div class="dm-view-header">
            <span class="dm-view-header-icon" aria-hidden="true">\u2194</span>
            <span class="dm-view-header-label">${o}</span>
            <span class="dm-view-header-badge">read-only</span>
        </div>
        <div class="dm-thread" ref=${e}>
            ${i.length===0&&f`
                <div class="empty-state">No messages in this conversation yet.</div>
            `}
            ${i.map(e=>{if(e.type===`dm_ended`){let t=`Conversation ended -- ${e.reason||`ended`}`;return f`<${km} key=${e.id} text=${t} />`}if(e.type===`system`)return f`<${km} key=${e.id} text=${e.text} />`;if(e.type===`notification`){let t=e.metadata||{};if(t.type===`dm_ended_notification`){let n=`DM with ${t.peer||`unknown`} ended -- ${Ac[t.reason]||t.reason||`ended`}`;return f`<${km} key=${e.id} text=${n} />`}return f`<${km} key=${e.id} text=${e.text} />`}if(e.type===`error`)return f`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-error">${e.text}</div></div>`;if(e.type===`tokens`)return null;if(e.type===`thinking`){let t=`Thinking…`;if(e.pending)t=`Sending…`;else if(e.queuedBehind>0)t=`Queued \u2014 position ${e.queuedBehind}\u2026`;else if(e.source){let n=e.source.startsWith(`peer:`)?e.source.slice(5):e.source;n&&(t=`${n} is thinking\u2026`)}return f`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-thinking">${t}</div></div>`}if(e.type===`warning`)return f`<${km} key=${e.id} text=${e.text||`Warning`} />`;if(e.type===`run_boundary`){if(!e.status||e.status===`completed`)return null;let t=e.status===`failed`?`run failed`:e.status===`cancelled`?`run cancelled`:`run ${e.status}`;return f`<${km} key=${e.id} text=${t} />`}if(e.type===`subagent_completed`){let t=`Subagent '${e.name||`subagent`}' ${e.status===`fail`?`failed`:`completed`}`;return f`<${km} key=${e.id} text=${t} />`}if(e.type===`job_completed`)return f`<${km} key=${e.id} text=${`Job '${e.jobName||`job`}' ${e.status||`completed`}`} />`;if(e.type===`context_debug`)return f`<${bf} key=${e.id} ...${e} />`;if(e.type===`dm_reasoning`)return f`<${jm} key=${e.id} ...${e} />`;if(e.type===`tool`){if(e.tool===`send_message`&&e.status===`done`&&!e.error)return null;Em.has(e.id)||(Em.add(e.id),console.warn(`[DmConversationView] ungrouped DM tool rendered as a standalone sibling row — this fallback is meant to be dead post-#1076/#1154. Tool:`,e.tool,`id:`,e.id,`runId:`,e.runId));let t=Dm({type:`agent`,role:`assistant`},r,a),n=t===`left`?r[0]:r[1];return f`
                        <div key=${e.id} class="dm-msg dm-msg-${t} dm-msg-tool-row">
                            <div class="dm-msg-name">${n||`?`}</div>
                            <${mf} ...${e} />
                        </div>
                    `}if(e.type===`image`){let t=Dm(e,r,a),n=e.fromAgent||(t===`left`?r[0]:r[1])||`?`;return f`
                        <div key=${e.id} class="dm-msg dm-msg-${t}">
                            <div class="dm-msg-name-row dm-msg-name-row-${t}">
                                <div class="dm-msg-name">${n}</div>
                                <${xd} ts=${e.ts} />
                            </div>
                            <div class="dm-msg-bubble">
                                ${e.url?f`<img src=${e.url} alt=${e.alt||``} class="dm-msg-image" />`:`[Image${e.alt?`: `+e.alt:``}]`}
                            </div>
                        </div>
                    `}return e.type===`user`||e.type===`agent`?f`<${Om} key=${e.id} msg=${e} participants=${r} perspectiveAgent=${a} />`:null})}
        </div>
        <div class="dm-view-footer">
            ${l?f`
                    <button class="dm-cancel-btn"
                            disabled=${u}
                            title="Stop this DM conversation"
                            aria-label="Stop conversation"
                            onClick=${Mm}>
                        <span class="dm-cancel-btn-icon" aria-hidden="true">\u25A0</span>
                        ${u?`Stopping…`:`Stop conversation`}
                    </button>
                `:f`
                    <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
                `}
        </div>
    `}function Pm(){return jc.value?f`
        <button
            type="button"
            class="stream-dead-banner"
            role="alert"
            aria-live="polite"
            onClick=${zc}
            title="Click to reconnect live updates"
        >
            <span class="stream-dead-banner-icon" aria-hidden="true">⚠</span>
            <span class="stream-dead-banner-text">
                Live updates disconnected — click to reconnect or reload.
            </span>
        </button>
    `:null}t(()=>{let e=A.value;document.title=e?`ALMS - ${e.name}`:`ALMS`});var Fm=d(`connecting...`);function Im(e){let t=[],n=0;for(;n<e.length;)if(e[n].type===`tool`){let r=[];for(;n<e.length&&e[n].type===`tool`;)r.push(e[n]),n++;r.length>1?t.push({_isToolGroup:!0,key:`tg-`+r[0].id,tools:r}):t.push(r[0])}else t.push(e[n]),n++;return t}function Lm(){let e=n(null);p(()=>{let n=0,r=t(()=>{F.value,cancelAnimationFrame(n),n=requestAnimationFrame(()=>{ad(e.current)})});return()=>{cancelAnimationFrame(n),r()}},[]);let r=Im(F.value),i=y.value,a=h.value,o=he.value,s=C.value,c=E.value,l=o?s?.agent_name?s.agent_name+` notifications`:`Notification session`:s?.session_type===`job`?c?c+` job session`:`Job session`:s?.session_type===`subagent`?`Subagent session`:`Internal session`,u=o?`⚡`:s?.session_type===`job`?`⏰`:`⚙`,d=s?.session_type?`internal-session-`+s.session_type:``;return f`
        <div id="chat">
            <${wm} />
            ${(js.value||As.value)&&f`
                <div id="messages" role="log" aria-live="polite">
                    ${js.value?f`<div class="loading-state">Loading agent...</div>`:f`<div class="loading-state">Loading session...</div>`}
                </div>
            `}
            ${!js.value&&!As.value&&i&&f`
                <${Nm} />
            `}
            ${!js.value&&!As.value&&!i&&f`
            ${a&&f`
                <div class="internal-session-header ${d}">
                    <span class="internal-session-header-icon" aria-hidden="true">${u}</span>
                    <span class="internal-session-header-label">${l}</span>
                    <span class="internal-session-header-badge">read-only</span>
                </div>
            `}
            ${tc.value&&f`
                <div class="sa-breadcrumb">
                    <button class="sa-breadcrumb-btn" onClick=${()=>bc()}>
                        \u2190 Back to parent session
                    </button>
                    ${M.value&&(Vs(x.value)?f`
                            <span class="sa-cancel-confirm-group sa-breadcrumb-cancel" role="group"
                                  aria-label="Confirm cancel subagent"
                                  onKeyDown=${e=>{e.key===`Escape`&&(e.preventDefault(),Us())}}>
                                <span class="sa-cancel-confirm-label">Cancel this subagent?</span>
                                <button class="sa-confirm-btn sa-confirm-yes"
                                        title="Yes, cancel this subagent"
                                        onClick=${()=>Gs(x.value)}>Yes</button>
                                <button class="sa-confirm-btn sa-confirm-no"
                                        title="No, keep it running"
                                        onClick=${()=>Us()}>No</button>
                            </span>
                        `:f`
                            <button class="sa-breadcrumb-cancel-btn sa-breadcrumb-cancel"
                                    title="Cancel this subagent"
                                    onClick=${()=>Hs(x.value)}>
                                Cancel subagent
                            </button>
                        `)}
                </div>
            `}
            <div id="messages" role="log" aria-live="polite" ref=${e}>
                ${F.value.length===0&&f`
                    <div class="empty-state">
                        ${a?`No activity recorded in this session yet.`:`No messages yet. Send a message to start.`}
                    </div>
                `}
                ${r.map(e=>{if(e._isToolGroup)return f`
                            <${hf} key=${e.key} count=${e.tools.length}>
                                ${e.tools.map(e=>f`<${mf} key=${e.id} ...${e} />`)}
                            <//>
                        `;let t=e;if(t.type===`user`||t.type===`agent`)return f`<${wd} key=${t.id} type=${t.type} text=${t.text} sealed=${t.sealed} fromAgent=${t.fromAgent} reasoning=${t.reasoning} ts=${t.ts} />`;if(t.type===`tool`)return f`<${mf} key=${t.id} ...${t} />`;if(t.type===`context_debug`)return f`<${bf} key=${t.id} ...${t} />`;if(t.type===`approval`)return f`<${Sf} key=${t.id} ...${t} />`;if(t.type===`job_completed`)return f`<${jf} key=${t.id} jobName=${t.jobName} status=${t.status} summary=${t.summary} ts=${t.ts} runId=${t.runId} truncated=${t.truncated} jobSessionUuid=${t.jobSessionUuid} jobSessionId=${t.jobSessionId} />`;if(t.type===`subagent_completed`)return f`<${If} key=${t.id}
                            name=${t.name} task=${t.task} status=${t.status}
                            toolCount=${t.toolCount} durationMs=${t.durationMs}
                            sessionId=${t.sessionId} summary=${t.summary} />`;if(t.type===`image`){let e=!!t.fromAgent,n=t.role===`user`&&!e?`user`:`agent`,r=E.value||A.value?.name,i=t.role===`user`&&!e?`>`:t.fromAgent?`${t.fromAgent} $`:r?`${r} $`:`$`;return f`
                            <div key=${t.id} class="msg ${n}">
                                <div class="msg-label-row">
                                    <div class="msg-label">${i}</div>
                                    ${t.ts&&f`<${xd} ts=${t.ts} />`}
                                </div>
                                <div class="msg-body">
                                    ${t.url?f`<img src=${t.url} alt=${t.alt||``} style="max-width:100%;border-radius:8px;" />`:`[Image${t.alt?`: `+t.alt:``}]`}
                                    ${t.alt&&f`<div style="font-size:var(--text-xs);color:var(--text-secondary);margin-top:var(--space-2);">${t.alt}</div>`}
                                </div>
                            </div>
                        `}if(t.type===`error`)return f`<${Ed} key=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`warning`)return f`<${Dd} key=${t.id} id=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`run_boundary`)return f`<${kd} key=${t.id} status=${t.status} error=${t.error} />`;if(t.type===`system`)return f`<${Od} key=${t.id} text=${t.text} />`;if(t.type===`dm_ended`)return f`<${Ad} key=${t.id} peer=${t.peer} reason=${t.reason} />`;if(t.type===`notification`){let e=t.metadata||{};return e.type===`dm_ended_notification`?f`<${Ad} key=${t.id} peer=${e.peer||`unknown`} reason=${Ac[e.reason]||e.reason||`conversation ended`} />`:f`<${Od} key=${t.id} text=${t.text} />`}if(t.type===`tokens`)return f`<${Td} key=${t.id} usage=${t.usage} />`;if(t.type===`thinking`){let e=`Thinking`,n=`thinking-indicator`;t.pending?(e=`Sending`,n=`pending-indicator`):t.queuedBehind>0?(e=`Queued \u2014 position ${t.queuedBehind}`,n=`queued-indicator`):t.source&&t.source.startsWith(`peer:`)?e=`Replying to message from `+t.source.slice(5):t.source===`job`?e=`Running scheduled job`:t.source===`subagent`&&(e=`Processing subagent result`);let r=A.value?.name||`Agent`;return f`
                            <div key=${t.id} class="msg agent">
                                <div class="msg-label">${r} $</div>
                                <div class="msg-body ${n}">${e}</div>
                            </div>
                        `}return null})}
            </div>
            <${Rf} />
            <${Cm} />
            ${a?f`
                    <div class="internal-session-footer">
                        <span class="internal-session-footer-text">This is a read-only view of internal agent activity.</span>
                    </div>
                `:f`<${Wf} />`}
            `}
        </div>
    `}function Rm(){let e=u(!1);return f`
        <${Iu} status=${Fm} onOpenSettings=${()=>{e.value=!0}} />
        <${Pm} />
        ${k.value.length>0?f`
                <div id="main">
                    <${td} />
                    <${Lm} />
                    <${fm} />
                </div>`:f`<${xm} />`}
        <${bm} open=${e.value} onClose=${()=>{e.value=!1}} />
    `}i(f`<${Rm} />`,document.getElementById(`app`));function zm(){Ms.value=!1,Fm.value=`connecting...`,du().then(()=>{Fm.value=`connected`}).catch(()=>{Fm.value=`offline`,Ms.value=!0})}Ps(zm),Vc(),zm();