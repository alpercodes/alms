const __vite__mapDeps=(i,m=__vite__mapDeps,d=(m.f||(m.f=["assets/chat-actions-UbdvPXnD.js","assets/rolldown-runtime-DK3Fl9T5.js","assets/deps-DqdW5C85.js","assets/runs-D_HMln2i.js","assets/select-generation-DvILpFQd.js","assets/agents-Dxvrmzg8.js"])))=>i.map(i=>d[i]);
import{t as e}from"./rolldown-runtime-DK3Fl9T5.js";import{a as t,c as n,d as r,f as i,i as a,l as o,n as s,o as c,p as l,r as u,s as d,t as f,u as p}from"./deps-DqdW5C85.js";import{A as m,C as h,D as g,E as _,O as v,S as y,T as b,_ as x,a as ee,b as S,c as te,d as ne,f as re,g as ie,h as ae,i as oe,j as se,k as ce,l as le,m as ue,n as de,o as fe,p as C,r as pe,s as me,t as he,u as ge,v as _e,w as ve,x as ye,y as w}from"./composer-storage-J651XfM0.js";import{n as T,r as E,t as D}from"./agents-Dxvrmzg8.js";import{i as be,n as xe,o as Se,r as Ce,t as O}from"./runs-D_HMln2i.js";import{a as we,c as k,i as Te,o as Ee,r as De,s as A,t as j}from"./chat-actions-UbdvPXnD.js";import{n as Oe,t as ke}from"./select-generation-DvILpFQd.js";(function(){let e=document.createElement(`link`).relList;if(e&&e.supports&&e.supports(`modulepreload`))return;for(let e of document.querySelectorAll(`link[rel="modulepreload"]`))n(e);new MutationObserver(e=>{for(let t of e)if(t.type===`childList`)for(let e of t.addedNodes)e.tagName===`LINK`&&e.rel===`modulepreload`&&n(e)}).observe(document,{childList:!0,subtree:!0});function t(e){let t={};return e.integrity&&(t.integrity=e.integrity),e.referrerPolicy&&(t.referrerPolicy=e.referrerPolicy),e.crossOrigin===`use-credentials`?t.credentials=`include`:e.crossOrigin===`anonymous`?t.credentials=`omit`:t.credentials=`same-origin`,t}function n(e){if(e.ep)return;e.ep=!0;let n=t(e);fetch(e.href,n)}})();var Ae=0;Array.isArray;function je(e,t,n,r,i,a){t||={};var o,s,c=t;if(`ref`in c)for(s in c={},t)s==`ref`?o=t[s]:c[s]=t[s];var u={type:e,props:c,key:n,ref:o,__k:null,__:null,__b:0,__e:null,__c:null,constructor:void 0,__v:--Ae,__i:-1,__u:0,__source:i,__self:a};if(typeof e==`function`&&(o=e.defaultProps))for(s in o)c[s]===void 0&&(c[s]=o[s]);return l.vnode&&l.vnode(u),u}function Me({message:e}){return je(`div`,{class:`contract-error-banner`,role:`alert`,children:[je(`strong`,{children:`Live data rejected`}),je(`span`,{children:e})]})}var Ne=null;function Pe(e){(Ne===null||!Ne.isConnected)&&(Ne=document.createElement(`div`),Ne.dataset.almsContractBoundary=`true`,document.body.prepend(Ne)),i(je(Me,{message:e}),Ne)}var Fe;function M(e,t,n){function r(n,r){if(n._zod||Object.defineProperty(n,"_zod",{value:{def:r,constr:o,traits:new Set},enumerable:!1}),n._zod.traits.has(e))return;n._zod.traits.add(e),t(n,r);let i=o.prototype,a=Object.keys(i);for(let e=0;e<a.length;e++){let t=a[e];t in n||(n[t]=i[t].bind(n))}}let i=n?.Parent??Object;class a extends i{}Object.defineProperty(a,"name",{value:e});function o(e){var t;let i=n?.Parent?new a:this;r(i,e),(t=i._zod).deferred??(t.deferred=[]);for(let e of i._zod.deferred)e();return i}return Object.defineProperty(o,"init",{value:r}),Object.defineProperty(o,Symbol.hasInstance,{value:t=>n?.Parent&&t instanceof n.Parent?!0:t?._zod?.traits?.has(e)}),Object.defineProperty(o,"name",{value:e}),o}var Ie=class extends Error{constructor(){super(`Encountered Promise during synchronous parse. Use .parseAsync() instead.`)}},Le=class extends Error{constructor(e){super(`Encountered unidirectional transform during encode: ${e}`),this.name=`ZodEncodeError`}};(Fe=globalThis).__zod_globalConfig??(Fe.__zod_globalConfig={});var Re=globalThis.__zod_globalConfig;function ze(e){return e&&Object.assign(Re,e),Re}function Be(e){let t=Object.values(e).filter(e=>typeof e==`number`);return Object.entries(e).filter(([e,n])=>t.indexOf(+e)===-1).map(([e,t])=>t)}function Ve(e,t){return typeof t==`bigint`?t.toString():t}function He(e){return{get value(){{let t=e();return Object.defineProperty(this,"value",{value:t}),t}throw Error(`cached value already set`)}}}function Ue(e){return e==null}function We(e){let t=+!!e.startsWith(`^`),n=e.endsWith(`$`)?e.length-1:e.length;return e.slice(t,n)}function Ge(e,t){let n=e/t,r=Math.round(n),i=2**-52*Math.max(Math.abs(n),1);return Math.abs(n-r)<i?0:n-r}var Ke=Symbol(`evaluating`);function N(e,t,n){let r;Object.defineProperty(e,t,{get(){if(r!==Ke)return r===void 0&&(r=Ke,r=n()),r},set(n){Object.defineProperty(e,t,{value:n})},configurable:!0})}function qe(e,t,n){Object.defineProperty(e,t,{value:n,writable:!0,enumerable:!0,configurable:!0})}function Je(...e){let t={};for(let n of e){let e=Object.getOwnPropertyDescriptors(n);Object.assign(t,e)}return Object.defineProperties({},t)}function Ye(e){return JSON.stringify(e)}function Xe(e){return e.toLowerCase().trim().replace(/[^\w\s-]/g,``).replace(/[\s_-]+/g,`-`).replace(/^-+|-+$/g,``)}var Ze=`captureStackTrace`in Error?Error.captureStackTrace:(...e)=>{};function Qe(e){return typeof e==`object`&&!!e&&!Array.isArray(e)}var $e=He(()=>{if(Re.jitless||typeof navigator<`u`&&navigator?.userAgent?.includes(`Cloudflare`))return!1;try{return Function(``),!0}catch{return!1}});function et(e){if(Qe(e)===!1)return!1;let t=e.constructor;if(t===void 0||typeof t!=`function`)return!0;let n=t.prototype;return!(Qe(n)===!1||Object.prototype.hasOwnProperty.call(n,`isPrototypeOf`)===!1)}function tt(e){return et(e)?{...e}:Array.isArray(e)?[...e]:e instanceof Map?new Map(e):e instanceof Set?new Set(e):e}var nt=new Set([`string`,`number`,`symbol`]);function rt(e){return e.replace(/[.*+?^${}()|[\]\\]/g,`\\$&`)}function it(e,t,n){let r=new e._zod.constr(t??e._zod.def);return(!t||n?.parent)&&(r._zod.parent=e),r}function P(e){let t=e;if(!t)return{};if(typeof t==`string`)return{error:()=>t};if(t?.message!==void 0){if(t?.error!==void 0)throw Error("Cannot specify both `message` and `error` params");t.error=t.message}return delete t.message,typeof t.error==`string`?{...t,error:()=>t.error}:t}function at(e){return Object.keys(e).filter(t=>e[t]._zod.optin===`optional`&&e[t]._zod.optout===`optional`)}var ot={safeint:[-(2**53-1),2**53-1],int32:[-2147483648,2147483647],uint32:[0,4294967295],float32:[-34028234663852886e22,34028234663852886e22],float64:[-Number.MAX_VALUE,Number.MAX_VALUE]};function st(e,t){let n=e._zod.def,r=n.checks;if(r&&r.length>0)throw Error(`.pick() cannot be used on object schemas containing refinements`);return it(e,Je(e._zod.def,{get shape(){let e={};for(let r in t){if(!(r in n.shape))throw Error(`Unrecognized key: "${r}"`);t[r]&&(e[r]=n.shape[r])}return qe(this,`shape`,e),e},checks:[]}))}function ct(e,t){let n=e._zod.def,r=n.checks;if(r&&r.length>0)throw Error(`.omit() cannot be used on object schemas containing refinements`);return it(e,Je(e._zod.def,{get shape(){let r={...e._zod.def.shape};for(let e in t){if(!(e in n.shape))throw Error(`Unrecognized key: "${e}"`);t[e]&&delete r[e]}return qe(this,`shape`,r),r},checks:[]}))}function lt(e,t){if(!et(t))throw Error(`Invalid input to extend: expected a plain object`);let n=e._zod.def.checks;if(n&&n.length>0){let n=e._zod.def.shape;for(let e in t)if(Object.getOwnPropertyDescriptor(n,e)!==void 0)throw Error("Cannot overwrite keys on object schemas containing refinements. Use `.safeExtend()` instead.")}return it(e,Je(e._zod.def,{get shape(){let n={...e._zod.def.shape,...t};return qe(this,`shape`,n),n}}))}function ut(e,t){if(!et(t))throw Error(`Invalid input to safeExtend: expected a plain object`);return it(e,Je(e._zod.def,{get shape(){let n={...e._zod.def.shape,...t};return qe(this,`shape`,n),n}}))}function dt(e,t){if(e._zod.def.checks?.length)throw Error(`.merge() cannot be used on object schemas containing refinements. Use .safeExtend() instead.`);return it(e,Je(e._zod.def,{get shape(){let n={...e._zod.def.shape,...t._zod.def.shape};return qe(this,`shape`,n),n},get catchall(){return t._zod.def.catchall},checks:t._zod.def.checks??[]}))}function ft(e,t,n){let r=t._zod.def.checks;if(r&&r.length>0)throw Error(`.partial() cannot be used on object schemas containing refinements`);return it(t,Je(t._zod.def,{get shape(){let r=t._zod.def.shape,i={...r};if(n)for(let t in n){if(!(t in r))throw Error(`Unrecognized key: "${t}"`);n[t]&&(i[t]=e?new e({type:`optional`,innerType:r[t]}):r[t])}else for(let t in r)i[t]=e?new e({type:`optional`,innerType:r[t]}):r[t];return qe(this,`shape`,i),i},checks:[]}))}function pt(e,t,n){return it(t,Je(t._zod.def,{get shape(){let r=t._zod.def.shape,i={...r};if(n)for(let t in n){if(!(t in i))throw Error(`Unrecognized key: "${t}"`);n[t]&&(i[t]=new e({type:`nonoptional`,innerType:r[t]}))}else for(let t in r)i[t]=new e({type:`nonoptional`,innerType:r[t]});return qe(this,`shape`,i),i}}))}function mt(e,t=0){if(e.aborted===!0)return!0;for(let n=t;n<e.issues.length;n++)if(e.issues[n]?.continue!==!0)return!0;return!1}function ht(e,t=0){if(e.aborted===!0)return!0;for(let n=t;n<e.issues.length;n++)if(e.issues[n]?.continue===!1)return!0;return!1}function gt(e,t){return t.map(t=>{var n;return(n=t).path??(n.path=[]),t.path.unshift(e),t})}function _t(e){return typeof e==`string`?e:e?.message}function vt(e,t,n){let r=e.message?e.message:_t(e.inst?._zod.def?.error?.(e))??_t(t?.error?.(e))??_t(n.customError?.(e))??_t(n.localeError?.(e))??`Invalid input`,{inst:i,continue:a,input:o,...s}=e;return s.path??=[],s.message=r,t?.reportInput&&(s.input=o),s}function yt(e){return Array.isArray(e)?`array`:typeof e==`string`?`string`:`unknown`}function bt(...e){let[t,n,r]=e;return typeof t==`string`?{message:t,code:`custom`,input:n,inst:r}:{...t}}var xt=(e,t)=>{e.name=`$ZodError`,Object.defineProperty(e,"_zod",{value:e._zod,enumerable:!1}),Object.defineProperty(e,"issues",{value:t,enumerable:!1}),e.message=JSON.stringify(t,Ve,2),Object.defineProperty(e,"toString",{value:()=>e.message,enumerable:!1})},St=M(`$ZodError`,xt),Ct=M(`$ZodError`,xt,{Parent:Error});function wt(e,t=e=>e.message){let n={},r=[];for(let i of e.issues)i.path.length>0?(n[i.path[0]]=n[i.path[0]]||[],n[i.path[0]].push(t(i))):r.push(t(i));return{formErrors:r,fieldErrors:n}}function Tt(e,t=e=>e.message){let n={_errors:[]},r=(e,i=[])=>{for(let a of e.issues)if(a.code===`invalid_union`&&a.errors.length)a.errors.map(e=>r({issues:e},[...i,...a.path]));else if(a.code===`invalid_key`)r({issues:a.issues},[...i,...a.path]);else if(a.code===`invalid_element`)r({issues:a.issues},[...i,...a.path]);else{let e=[...i,...a.path];if(e.length===0)n._errors.push(t(a));else{let r=n,i=0;for(;i<e.length;){let n=e[i];i===e.length-1?(r[n]=r[n]||{_errors:[]},r[n]._errors.push(t(a))):r[n]=r[n]||{_errors:[]},r=r[n],i++}}}};return r(e),n}var Et=e=>(t,n,r,i)=>{let a=r?{...r,async:!1}:{async:!1},o=t._zod.run({value:n,issues:[]},a);if(o instanceof Promise)throw new Ie;if(o.issues.length){let t=new((i?.Err)??e)(o.issues.map(e=>vt(e,a,ze())));throw Ze(t,i?.callee),t}return o.value},Dt=e=>async(t,n,r,i)=>{let a=r?{...r,async:!0}:{async:!0},o=t._zod.run({value:n,issues:[]},a);if(o instanceof Promise&&(o=await o),o.issues.length){let t=new((i?.Err)??e)(o.issues.map(e=>vt(e,a,ze())));throw Ze(t,i?.callee),t}return o.value},Ot=e=>(t,n,r)=>{let i=r?{...r,async:!1}:{async:!1},a=t._zod.run({value:n,issues:[]},i);if(a instanceof Promise)throw new Ie;return a.issues.length?{success:!1,error:new(e??St)(a.issues.map(e=>vt(e,i,ze())))}:{success:!0,data:a.value}},kt=Ot(Ct),At=e=>async(t,n,r)=>{let i=r?{...r,async:!0}:{async:!0},a=t._zod.run({value:n,issues:[]},i);return a instanceof Promise&&(a=await a),a.issues.length?{success:!1,error:new e(a.issues.map(e=>vt(e,i,ze())))}:{success:!0,data:a.value}},jt=At(Ct),Mt=e=>(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return Et(e)(t,n,i)},Nt=e=>(t,n,r)=>Et(e)(t,n,r),Pt=e=>async(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return Dt(e)(t,n,i)},Ft=e=>async(t,n,r)=>Dt(e)(t,n,r),It=e=>(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return Ot(e)(t,n,i)},Lt=e=>(t,n,r)=>Ot(e)(t,n,r),Rt=e=>async(t,n,r)=>{let i=r?{...r,direction:`backward`}:{direction:`backward`};return At(e)(t,n,i)},zt=e=>async(t,n,r)=>At(e)(t,n,r),Bt=/^[cC][0-9a-z]{6,}$/,Vt=/^[0-9a-z]+$/,Ht=/^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$/,Ut=/^[0-9a-vA-V]{20}$/,Wt=/^[A-Za-z0-9]{27}$/,Gt=/^[a-zA-Z0-9_-]{21}$/,Kt=/^P(?:(\d+W)|(?!.*W)(?=\d|T\d)(\d+Y)?(\d+M)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+([.,]\d+)?S)?)?)$/,qt=/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/,Jt=e=>e?RegExp(`^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-${e}[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12})$`):/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}|00000000-0000-0000-0000-000000000000|ffffffff-ffff-ffff-ffff-ffffffffffff)$/,Yt=/^(?!\.)(?!.*\.\.)([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$/,Xt=`^(\\p{Extended_Pictographic}|\\p{Emoji_Component})+$`;function Zt(){return new RegExp(Xt,`u`)}var Qt=/^(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])$/,$t=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:))$/,en=/^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\/([0-9]|[1-2][0-9]|3[0-2])$/,tn=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|::|([0-9a-fA-F]{1,4})?::([0-9a-fA-F]{1,4}:?){0,6})\/(12[0-8]|1[01][0-9]|[1-9]?[0-9])$/,nn=/^$|^(?:[0-9a-zA-Z+/]{4})*(?:(?:[0-9a-zA-Z+/]{2}==)|(?:[0-9a-zA-Z+/]{3}=))?$/,rn=/^[A-Za-z0-9_-]*$/,an=/^https?$/,on=/^\+[1-9]\d{6,14}$/,sn=`(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))`,cn=RegExp(`^${sn}$`);function ln(e){let t=`(?:[01]\\d|2[0-3]):[0-5]\\d`;return typeof e.precision==`number`?e.precision===-1?`${t}`:e.precision===0?`${t}:[0-5]\\d`:`${t}:[0-5]\\d\\.\\d{${e.precision}}`:`${t}(?::[0-5]\\d(?:\\.\\d+)?)?`}function un(e){return RegExp(`^${ln(e)}$`)}function dn(e){let t=ln({precision:e.precision}),n=[`Z`];e.local&&n.push(``),e.offset&&n.push(`([+-](?:[01]\\d|2[0-3]):[0-5]\\d)`);let r=`${t}(?:${n.join(`|`)})`;return RegExp(`^${sn}T(?:${r})$`)}var fn=e=>{let t=e?`[\\s\\S]{${e?.minimum??0},${e?.maximum??``}}`:`[\\s\\S]*`;return RegExp(`^${t}$`)},pn=/^-?\d+$/,mn=/^-?\d+(?:\.\d+)?$/,hn=/^(?:true|false)$/i,gn=/^null$/i,_n=/^[^A-Z]*$/,vn=/^[^a-z]*$/,yn=M(`$ZodCheck`,(e,t)=>{var n;e._zod??={},e._zod.def=t,(n=e._zod).onattach??(n.onattach=[])}),bn={number:`number`,bigint:`bigint`,object:`date`},xn=M(`$ZodCheckLessThan`,(e,t)=>{yn.init(e,t);let n=bn[typeof t.value];e._zod.onattach.push(e=>{let n=e._zod.bag,r=(t.inclusive?n.maximum:n.exclusiveMaximum)??1/0;t.value<r&&(t.inclusive?n.maximum=t.value:n.exclusiveMaximum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value<=t.value:r.value<t.value)||r.issues.push({origin:n,code:`too_big`,maximum:typeof t.value==`object`?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),Sn=M(`$ZodCheckGreaterThan`,(e,t)=>{yn.init(e,t);let n=bn[typeof t.value];e._zod.onattach.push(e=>{let n=e._zod.bag,r=(t.inclusive?n.minimum:n.exclusiveMinimum)??-1/0;t.value>r&&(t.inclusive?n.minimum=t.value:n.exclusiveMinimum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value>=t.value:r.value>t.value)||r.issues.push({origin:n,code:`too_small`,minimum:typeof t.value==`object`?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),Cn=M(`$ZodCheckMultipleOf`,(e,t)=>{yn.init(e,t),e._zod.onattach.push(e=>{var n;(n=e._zod.bag).multipleOf??(n.multipleOf=t.value)}),e._zod.check=n=>{if(typeof n.value!=typeof t.value)throw Error(`Cannot mix number and bigint in multiple_of check.`);(typeof n.value==`bigint`?n.value%t.value===BigInt(0):Ge(n.value,t.value)===0)||n.issues.push({origin:typeof n.value,code:`not_multiple_of`,divisor:t.value,input:n.value,inst:e,continue:!t.abort})}}),wn=M(`$ZodCheckNumberFormat`,(e,t)=>{yn.init(e,t),t.format=t.format||`float64`;let n=t.format?.includes(`int`),r=n?`int`:`number`,[i,a]=ot[t.format];e._zod.onattach.push(e=>{let r=e._zod.bag;r.format=t.format,r.minimum=i,r.maximum=a,n&&(r.pattern=pn)}),e._zod.check=o=>{let s=o.value;if(n){if(!Number.isInteger(s)){o.issues.push({expected:r,format:t.format,code:`invalid_type`,continue:!1,input:s,inst:e});return}if(!Number.isSafeInteger(s)){s>0?o.issues.push({input:s,code:`too_big`,maximum:2**53-1,note:`Integers must be within the safe integer range.`,inst:e,origin:r,inclusive:!0,continue:!t.abort}):o.issues.push({input:s,code:`too_small`,minimum:-(2**53-1),note:`Integers must be within the safe integer range.`,inst:e,origin:r,inclusive:!0,continue:!t.abort});return}}s<i&&o.issues.push({origin:`number`,input:s,code:`too_small`,minimum:i,inclusive:!0,inst:e,continue:!t.abort}),s>a&&o.issues.push({origin:`number`,input:s,code:`too_big`,maximum:a,inclusive:!0,inst:e,continue:!t.abort})}}),Tn=M(`$ZodCheckMaxLength`,(e,t)=>{var n;yn.init(e,t),(n=e._zod.def).when??(n.when=e=>{let t=e.value;return!Ue(t)&&t.length!==void 0}),e._zod.onattach.push(e=>{let n=e._zod.bag.maximum??1/0;t.maximum<n&&(e._zod.bag.maximum=t.maximum)}),e._zod.check=n=>{let r=n.value;if(r.length<=t.maximum)return;let i=yt(r);n.issues.push({origin:i,code:`too_big`,maximum:t.maximum,inclusive:!0,input:r,inst:e,continue:!t.abort})}}),En=M(`$ZodCheckMinLength`,(e,t)=>{var n;yn.init(e,t),(n=e._zod.def).when??(n.when=e=>{let t=e.value;return!Ue(t)&&t.length!==void 0}),e._zod.onattach.push(e=>{let n=e._zod.bag.minimum??-1/0;t.minimum>n&&(e._zod.bag.minimum=t.minimum)}),e._zod.check=n=>{let r=n.value;if(r.length>=t.minimum)return;let i=yt(r);n.issues.push({origin:i,code:`too_small`,minimum:t.minimum,inclusive:!0,input:r,inst:e,continue:!t.abort})}}),Dn=M(`$ZodCheckLengthEquals`,(e,t)=>{var n;yn.init(e,t),(n=e._zod.def).when??(n.when=e=>{let t=e.value;return!Ue(t)&&t.length!==void 0}),e._zod.onattach.push(e=>{let n=e._zod.bag;n.minimum=t.length,n.maximum=t.length,n.length=t.length}),e._zod.check=n=>{let r=n.value,i=r.length;if(i===t.length)return;let a=yt(r),o=i>t.length;n.issues.push({origin:a,...o?{code:`too_big`,maximum:t.length}:{code:`too_small`,minimum:t.length},inclusive:!0,exact:!0,input:n.value,inst:e,continue:!t.abort})}}),On=M(`$ZodCheckStringFormat`,(e,t)=>{var n,r;yn.init(e,t),e._zod.onattach.push(e=>{let n=e._zod.bag;n.format=t.format,t.pattern&&(n.patterns??=new Set,n.patterns.add(t.pattern))}),t.pattern?(n=e._zod).check??(n.check=n=>{t.pattern.lastIndex=0,!t.pattern.test(n.value)&&n.issues.push({origin:`string`,code:`invalid_format`,format:t.format,input:n.value,...t.pattern?{pattern:t.pattern.toString()}:{},inst:e,continue:!t.abort})}):(r=e._zod).check??(r.check=()=>{})}),kn=M(`$ZodCheckRegex`,(e,t)=>{On.init(e,t),e._zod.check=n=>{t.pattern.lastIndex=0,!t.pattern.test(n.value)&&n.issues.push({origin:`string`,code:`invalid_format`,format:`regex`,input:n.value,pattern:t.pattern.toString(),inst:e,continue:!t.abort})}}),An=M(`$ZodCheckLowerCase`,(e,t)=>{t.pattern??=_n,On.init(e,t)}),jn=M(`$ZodCheckUpperCase`,(e,t)=>{t.pattern??=vn,On.init(e,t)}),Mn=M(`$ZodCheckIncludes`,(e,t)=>{yn.init(e,t);let n=rt(t.includes),r=new RegExp(typeof t.position==`number`?`^.{${t.position}}${n}`:n);t.pattern=r,e._zod.onattach.push(e=>{let t=e._zod.bag;t.patterns??=new Set,t.patterns.add(r)}),e._zod.check=n=>{n.value.includes(t.includes,t.position)||n.issues.push({origin:`string`,code:`invalid_format`,format:`includes`,includes:t.includes,input:n.value,inst:e,continue:!t.abort})}}),Nn=M(`$ZodCheckStartsWith`,(e,t)=>{yn.init(e,t);let n=RegExp(`^${rt(t.prefix)}.*`);t.pattern??=n,e._zod.onattach.push(e=>{let t=e._zod.bag;t.patterns??=new Set,t.patterns.add(n)}),e._zod.check=n=>{n.value.startsWith(t.prefix)||n.issues.push({origin:`string`,code:`invalid_format`,format:`starts_with`,prefix:t.prefix,input:n.value,inst:e,continue:!t.abort})}}),Pn=M(`$ZodCheckEndsWith`,(e,t)=>{yn.init(e,t);let n=RegExp(`.*${rt(t.suffix)}$`);t.pattern??=n,e._zod.onattach.push(e=>{let t=e._zod.bag;t.patterns??=new Set,t.patterns.add(n)}),e._zod.check=n=>{n.value.endsWith(t.suffix)||n.issues.push({origin:`string`,code:`invalid_format`,format:`ends_with`,suffix:t.suffix,input:n.value,inst:e,continue:!t.abort})}}),Fn=M(`$ZodCheckOverwrite`,(e,t)=>{yn.init(e,t),e._zod.check=e=>{e.value=t.tx(e.value)}}),In=class{constructor(e=[]){this.content=[],this.indent=0,this&&(this.args=e)}indented(e){this.indent+=1,e(this),--this.indent}write(e){if(typeof e==`function`){e(this,{execution:`sync`}),e(this,{execution:`async`});return}let t=e.split(`
`).filter(e=>e),n=Math.min(...t.map(e=>e.length-e.trimStart().length)),r=t.map(e=>e.slice(n)).map(e=>` `.repeat(this.indent*2)+e);for(let e of r)this.content.push(e)}compile(){let e=Function,t=this?.args,n=[...(this?.content??[``]).map(e=>`  ${e}`)];return new e(...t,n.join(`
`))}},Ln={major:4,minor:4,patch:3},F=M(`$ZodType`,(e,t)=>{var n;e??={},e._zod.def=t,e._zod.bag=e._zod.bag||{},e._zod.version=Ln;let r=[...e._zod.def.checks??[]];e._zod.traits.has(`$ZodCheck`)&&r.unshift(e);for(let t of r)for(let n of t._zod.onattach)n(e);if(r.length===0)(n=e._zod).deferred??(n.deferred=[]),e._zod.deferred?.push(()=>{e._zod.run=e._zod.parse});else{let t=(e,t,n)=>{let r=mt(e),i;for(let a of t){if(a._zod.def.when){if(ht(e)||!a._zod.def.when(e))continue}else if(r)continue;let t=e.issues.length,o=a._zod.check(e);if(o instanceof Promise&&n?.async===!1)throw new Ie;if(i||o instanceof Promise)i=(i??Promise.resolve()).then(async()=>{await o,e.issues.length!==t&&(r||=mt(e,t))});else{if(e.issues.length===t)continue;r||=mt(e,t)}}return i?i.then(()=>e):e},n=(n,i,a)=>{if(mt(n))return n.aborted=!0,n;let o=t(i,r,a);if(o instanceof Promise){if(a.async===!1)throw new Ie;return o.then(t=>e._zod.parse(t,a))}return e._zod.parse(o,a)};e._zod.run=(i,a)=>{if(a.skipChecks)return e._zod.parse(i,a);if(a.direction===`backward`){let t=e._zod.parse({value:i.value,issues:[]},{...a,skipChecks:!0});return t instanceof Promise?t.then(e=>n(e,i,a)):n(t,i,a)}let o=e._zod.parse(i,a);if(o instanceof Promise){if(a.async===!1)throw new Ie;return o.then(e=>t(e,r,a))}return t(o,r,a)}}N(e,`~standard`,()=>({validate:t=>{try{let n=kt(e,t);return n.success?{value:n.data}:{issues:n.error?.issues}}catch{return jt(e,t).then(e=>e.success?{value:e.data}:{issues:e.error?.issues})}},vendor:`zod`,version:1}))}),Rn=M(`$ZodString`,(e,t)=>{F.init(e,t),e._zod.pattern=[...e?._zod.bag?.patterns??[]].pop()??fn(e._zod.bag),e._zod.parse=(n,r)=>{if(t.coerce)try{n.value=String(n.value)}catch{}return typeof n.value==`string`||n.issues.push({expected:`string`,code:`invalid_type`,input:n.value,inst:e}),n}}),I=M(`$ZodStringFormat`,(e,t)=>{On.init(e,t),Rn.init(e,t)}),zn=M(`$ZodGUID`,(e,t)=>{t.pattern??=qt,I.init(e,t)}),Bn=M(`$ZodUUID`,(e,t)=>{if(t.version){let e={v1:1,v2:2,v3:3,v4:4,v5:5,v6:6,v7:7,v8:8}[t.version];if(e===void 0)throw Error(`Invalid UUID version: "${t.version}"`);t.pattern??=Jt(e)}else t.pattern??=Jt();I.init(e,t)}),Vn=M(`$ZodEmail`,(e,t)=>{t.pattern??=Yt,I.init(e,t)}),Hn=M(`$ZodURL`,(e,t)=>{I.init(e,t),e._zod.check=n=>{try{let r=n.value.trim();if(!t.normalize&&t.protocol?.source===an.source&&!/^https?:\/\//i.test(r)){n.issues.push({code:`invalid_format`,format:`url`,note:`Invalid URL format`,input:n.value,inst:e,continue:!t.abort});return}let i=new URL(r);t.hostname&&(t.hostname.lastIndex=0,t.hostname.test(i.hostname)||n.issues.push({code:`invalid_format`,format:`url`,note:`Invalid hostname`,pattern:t.hostname.source,input:n.value,inst:e,continue:!t.abort})),t.protocol&&(t.protocol.lastIndex=0,t.protocol.test(i.protocol.endsWith(`:`)?i.protocol.slice(0,-1):i.protocol)||n.issues.push({code:`invalid_format`,format:`url`,note:`Invalid protocol`,pattern:t.protocol.source,input:n.value,inst:e,continue:!t.abort})),t.normalize?n.value=i.href:n.value=r;return}catch{n.issues.push({code:`invalid_format`,format:`url`,input:n.value,inst:e,continue:!t.abort})}}}),Un=M(`$ZodEmoji`,(e,t)=>{t.pattern??=Zt(),I.init(e,t)}),Wn=M(`$ZodNanoID`,(e,t)=>{t.pattern??=Gt,I.init(e,t)}),Gn=M(`$ZodCUID`,(e,t)=>{t.pattern??=Bt,I.init(e,t)}),Kn=M(`$ZodCUID2`,(e,t)=>{t.pattern??=Vt,I.init(e,t)}),qn=M(`$ZodULID`,(e,t)=>{t.pattern??=Ht,I.init(e,t)}),Jn=M(`$ZodXID`,(e,t)=>{t.pattern??=Ut,I.init(e,t)}),Yn=M(`$ZodKSUID`,(e,t)=>{t.pattern??=Wt,I.init(e,t)}),Xn=M(`$ZodISODateTime`,(e,t)=>{t.pattern??=dn(t),I.init(e,t)}),Zn=M(`$ZodISODate`,(e,t)=>{t.pattern??=cn,I.init(e,t)}),Qn=M(`$ZodISOTime`,(e,t)=>{t.pattern??=un(t),I.init(e,t)}),$n=M(`$ZodISODuration`,(e,t)=>{t.pattern??=Kt,I.init(e,t)}),er=M(`$ZodIPv4`,(e,t)=>{t.pattern??=Qt,I.init(e,t),e._zod.bag.format=`ipv4`}),tr=M(`$ZodIPv6`,(e,t)=>{t.pattern??=$t,I.init(e,t),e._zod.bag.format=`ipv6`,e._zod.check=n=>{try{new URL(`http://[${n.value}]`)}catch{n.issues.push({code:`invalid_format`,format:`ipv6`,input:n.value,inst:e,continue:!t.abort})}}}),nr=M(`$ZodCIDRv4`,(e,t)=>{t.pattern??=en,I.init(e,t)}),rr=M(`$ZodCIDRv6`,(e,t)=>{t.pattern??=tn,I.init(e,t),e._zod.check=n=>{let r=n.value.split(`/`);try{if(r.length!==2)throw Error();let[e,t]=r;if(!t)throw Error();let n=Number(t);if(`${n}`!==t||n<0||n>128)throw Error();new URL(`http://[${e}]`)}catch{n.issues.push({code:`invalid_format`,format:`cidrv6`,input:n.value,inst:e,continue:!t.abort})}}});function ir(e){if(e===``)return!0;if(/\s/.test(e)||e.length%4!=0)return!1;try{return atob(e),!0}catch{return!1}}var ar=M(`$ZodBase64`,(e,t)=>{t.pattern??=nn,I.init(e,t),e._zod.bag.contentEncoding=`base64`,e._zod.check=n=>{ir(n.value)||n.issues.push({code:`invalid_format`,format:`base64`,input:n.value,inst:e,continue:!t.abort})}});function or(e){if(!rn.test(e))return!1;let t=e.replace(/[-_]/g,e=>e===`-`?`+`:`/`);return ir(t.padEnd(Math.ceil(t.length/4)*4,`=`))}var sr=M(`$ZodBase64URL`,(e,t)=>{t.pattern??=rn,I.init(e,t),e._zod.bag.contentEncoding=`base64url`,e._zod.check=n=>{or(n.value)||n.issues.push({code:`invalid_format`,format:`base64url`,input:n.value,inst:e,continue:!t.abort})}}),cr=M(`$ZodE164`,(e,t)=>{t.pattern??=on,I.init(e,t)});function lr(e,t=null){try{let n=e.split(`.`);if(n.length!==3)return!1;let[r]=n;if(!r)return!1;let i=JSON.parse(atob(r));return!(`typ`in i&&i?.typ!==`JWT`||!i.alg||t&&(!(`alg`in i)||i.alg!==t))}catch{return!1}}var ur=M(`$ZodJWT`,(e,t)=>{I.init(e,t),e._zod.check=n=>{lr(n.value,t.alg)||n.issues.push({code:`invalid_format`,format:`jwt`,input:n.value,inst:e,continue:!t.abort})}}),dr=M(`$ZodNumber`,(e,t)=>{F.init(e,t),e._zod.pattern=e._zod.bag.pattern??mn,e._zod.parse=(n,r)=>{if(t.coerce)try{n.value=Number(n.value)}catch{}let i=n.value;if(typeof i==`number`&&!Number.isNaN(i)&&Number.isFinite(i))return n;let a=typeof i==`number`?Number.isNaN(i)?`NaN`:Number.isFinite(i)?void 0:`Infinity`:void 0;return n.issues.push({expected:`number`,code:`invalid_type`,input:i,inst:e,...a?{received:a}:{}}),n}}),fr=M(`$ZodNumberFormat`,(e,t)=>{wn.init(e,t),dr.init(e,t)}),pr=M(`$ZodBoolean`,(e,t)=>{F.init(e,t),e._zod.pattern=hn,e._zod.parse=(n,r)=>{if(t.coerce)try{n.value=!!n.value}catch{}let i=n.value;return typeof i==`boolean`||n.issues.push({expected:`boolean`,code:`invalid_type`,input:i,inst:e}),n}}),mr=M(`$ZodNull`,(e,t)=>{F.init(e,t),e._zod.pattern=gn,e._zod.values=new Set([null]),e._zod.parse=(t,n)=>{let r=t.value;return r===null||t.issues.push({expected:`null`,code:`invalid_type`,input:r,inst:e}),t}}),hr=M(`$ZodUnknown`,(e,t)=>{F.init(e,t),e._zod.parse=e=>e}),gr=M(`$ZodNever`,(e,t)=>{F.init(e,t),e._zod.parse=(t,n)=>(t.issues.push({expected:`never`,code:`invalid_type`,input:t.value,inst:e}),t)});function _r(e,t,n){e.issues.length&&t.issues.push(...gt(n,e.issues)),t.value[n]=e.value}var vr=M(`$ZodArray`,(e,t)=>{F.init(e,t),e._zod.parse=(n,r)=>{let i=n.value;if(!Array.isArray(i))return n.issues.push({expected:`array`,code:`invalid_type`,input:i,inst:e}),n;n.value=Array(i.length);let a=[];for(let e=0;e<i.length;e++){let o=i[e],s=t.element._zod.run({value:o,issues:[]},r);s instanceof Promise?a.push(s.then(t=>_r(t,n,e))):_r(s,n,e)}return a.length?Promise.all(a).then(()=>n):n}});function yr(e,t,n,r,i,a){let o=n in r;if(e.issues.length){if(i&&a&&!o)return;t.issues.push(...gt(n,e.issues))}if(!o&&!i){e.issues.length||t.issues.push({code:`invalid_type`,expected:`nonoptional`,input:void 0,path:[n]});return}e.value===void 0?o&&(t.value[n]=void 0):t.value[n]=e.value}function br(e){let t=Object.keys(e.shape);for(let n of t)if(!e.shape?.[n]?._zod?.traits?.has(`$ZodType`))throw Error(`Invalid element at key "${n}": expected a Zod schema`);let n=at(e.shape);return{...e,keys:t,keySet:new Set(t),numKeys:t.length,optionalKeys:new Set(n)}}function xr(e,t,n,r,i,a){let o=[],s=i.keySet,c=i.catchall._zod,l=c.def.type,u=c.optin===`optional`,d=c.optout===`optional`;for(let i in t){if(i===`__proto__`||s.has(i))continue;if(l===`never`){o.push(i);continue}let a=c.run({value:t[i],issues:[]},r);a instanceof Promise?e.push(a.then(e=>yr(e,n,i,t,u,d))):yr(a,n,i,t,u,d)}return o.length&&n.issues.push({code:`unrecognized_keys`,keys:o,input:t,inst:a}),e.length?Promise.all(e).then(()=>n):n}var Sr=M(`$ZodObject`,(e,t)=>{if(F.init(e,t),!Object.getOwnPropertyDescriptor(t,`shape`)?.get){let e=t.shape;Object.defineProperty(t,"shape",{get:()=>{let n={...e};return Object.defineProperty(t,"shape",{value:n}),n}})}let n=He(()=>br(t));N(e._zod,`propValues`,()=>{let e=t.shape,n={};for(let t in e){let r=e[t]._zod;if(r.values){n[t]??(n[t]=new Set);for(let e of r.values)n[t].add(e)}}return n});let r=Qe,i=t.catchall,a;e._zod.parse=(t,o)=>{a??=n.value;let s=t.value;if(!r(s))return t.issues.push({expected:`object`,code:`invalid_type`,input:s,inst:e}),t;t.value={};let c=[],l=a.shape;for(let e of a.keys){let n=l[e],r=n._zod.optin===`optional`,i=n._zod.optout===`optional`,a=n._zod.run({value:s[e],issues:[]},o);a instanceof Promise?c.push(a.then(n=>yr(n,t,e,s,r,i))):yr(a,t,e,s,r,i)}return i?xr(c,s,t,o,n.value,e):c.length?Promise.all(c).then(()=>t):t}}),Cr=M(`$ZodObjectJIT`,(e,t)=>{Sr.init(e,t);let n=e._zod.parse,r=He(()=>br(t)),i=e=>{let t=new In([`shape`,`payload`,`ctx`]),n=r.value,i=e=>{let t=Ye(e);return`shape[${t}]._zod.run({ value: input[${t}], issues: [] }, ctx)`};t.write(`const input = payload.value;`);let a=Object.create(null),o=0;for(let e of n.keys)a[e]=`key_${o++}`;t.write(`const newResult = {};`);for(let r of n.keys){let n=a[r],o=Ye(r),s=e[r],c=s?._zod?.optin===`optional`,l=s?._zod?.optout===`optional`;t.write(`const ${n} = ${i(r)};`),c&&l?t.write(`
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

      `)}t.write(`payload.value = newResult;`),t.write(`return payload;`);let s=t.compile();return(t,n)=>s(e,t,n)},a,o=Qe,s=!Re.jitless,c=s&&$e.value,l=t.catchall,u;e._zod.parse=(d,f)=>{u??=r.value;let p=d.value;return o(p)?s&&c&&f?.async===!1&&f.jitless!==!0?(a||=i(t.shape),d=a(d,f),l?xr([],p,d,f,u,e):d):n(d,f):(d.issues.push({expected:`object`,code:`invalid_type`,input:p,inst:e}),d)}});function wr(e,t,n,r){for(let n of e)if(n.issues.length===0)return t.value=n.value,t;let i=e.filter(e=>!mt(e));return i.length===1?(t.value=i[0].value,i[0]):(t.issues.push({code:`invalid_union`,input:t.value,inst:n,errors:e.map(e=>e.issues.map(e=>vt(e,r,ze())))}),t)}var Tr=M(`$ZodUnion`,(e,t)=>{F.init(e,t),N(e._zod,`optin`,()=>t.options.some(e=>e._zod.optin===`optional`)?`optional`:void 0),N(e._zod,`optout`,()=>t.options.some(e=>e._zod.optout===`optional`)?`optional`:void 0),N(e._zod,`values`,()=>{if(t.options.every(e=>e._zod.values))return new Set(t.options.flatMap(e=>Array.from(e._zod.values)))}),N(e._zod,`pattern`,()=>{if(t.options.every(e=>e._zod.pattern)){let e=t.options.map(e=>e._zod.pattern);return RegExp(`^(${e.map(e=>We(e.source)).join(`|`)})$`)}});let n=t.options.length===1?t.options[0]._zod.run:null;e._zod.parse=(r,i)=>{if(n)return n(r,i);let a=!1,o=[];for(let e of t.options){let t=e._zod.run({value:r.value,issues:[]},i);if(t instanceof Promise)o.push(t),a=!0;else{if(t.issues.length===0)return t;o.push(t)}}return a?Promise.all(o).then(t=>wr(t,r,e,i)):wr(o,r,e,i)}}),Er=M(`$ZodDiscriminatedUnion`,(e,t)=>{t.inclusive=!1,Tr.init(e,t);let n=e._zod.parse;N(e._zod,`propValues`,()=>{let e={};for(let n of t.options){let r=n._zod.propValues;if(!r||Object.keys(r).length===0)throw Error(`Invalid discriminated union option at index "${t.options.indexOf(n)}"`);for(let[t,n]of Object.entries(r)){e[t]||(e[t]=new Set);for(let r of n)e[t].add(r)}}return e});let r=He(()=>{let e=t.options,n=new Map;for(let r of e){let e=r._zod.propValues?.[t.discriminator];if(!e||e.size===0)throw Error(`Invalid discriminated union option at index "${t.options.indexOf(r)}"`);for(let t of e){if(n.has(t))throw Error(`Duplicate discriminator value "${String(t)}"`);n.set(t,r)}}return n});e._zod.parse=(i,a)=>{let o=i.value;if(!Qe(o))return i.issues.push({code:`invalid_type`,expected:`object`,input:o,inst:e}),i;let s=r.value.get(o?.[t.discriminator]);return s?s._zod.run(i,a):t.unionFallback||a.direction===`backward`?n(i,a):(i.issues.push({code:`invalid_union`,errors:[],note:`No matching discriminator`,discriminator:t.discriminator,options:Array.from(r.value.keys()),input:o,path:[t.discriminator],inst:e}),i)}}),Dr=M(`$ZodIntersection`,(e,t)=>{F.init(e,t),e._zod.parse=(e,n)=>{let r=e.value,i=t.left._zod.run({value:r,issues:[]},n),a=t.right._zod.run({value:r,issues:[]},n);return i instanceof Promise||a instanceof Promise?Promise.all([i,a]).then(([t,n])=>kr(e,t,n)):kr(e,i,a)}});function Or(e,t){if(e===t||e instanceof Date&&t instanceof Date&&+e==+t)return{valid:!0,data:e};if(et(e)&&et(t)){let n=Object.keys(t),r=Object.keys(e).filter(e=>n.indexOf(e)!==-1),i={...e,...t};for(let n of r){let r=Or(e[n],t[n]);if(!r.valid)return{valid:!1,mergeErrorPath:[n,...r.mergeErrorPath]};i[n]=r.data}return{valid:!0,data:i}}if(Array.isArray(e)&&Array.isArray(t)){if(e.length!==t.length)return{valid:!1,mergeErrorPath:[]};let n=[];for(let r=0;r<e.length;r++){let i=e[r],a=t[r],o=Or(i,a);if(!o.valid)return{valid:!1,mergeErrorPath:[r,...o.mergeErrorPath]};n.push(o.data)}return{valid:!0,data:n}}return{valid:!1,mergeErrorPath:[]}}function kr(e,t,n){let r=new Map,i;for(let n of t.issues)if(n.code===`unrecognized_keys`){i??=n;for(let e of n.keys)r.has(e)||r.set(e,{}),r.get(e).l=!0}else e.issues.push(n);for(let t of n.issues)if(t.code===`unrecognized_keys`)for(let e of t.keys)r.has(e)||r.set(e,{}),r.get(e).r=!0;else e.issues.push(t);let a=[...r].filter(([,e])=>e.l&&e.r).map(([e])=>e);if(a.length&&i&&e.issues.push({...i,keys:a}),mt(e))return e;let o=Or(t.value,n.value);if(!o.valid)throw Error(`Unmergable intersection. Error path: ${JSON.stringify(o.mergeErrorPath)}`);return e.value=o.data,e}var Ar=M(`$ZodTuple`,(e,t)=>{F.init(e,t);let n=t.items;e._zod.parse=(r,i)=>{let a=r.value;if(!Array.isArray(a))return r.issues.push({input:a,inst:e,expected:`tuple`,code:`invalid_type`}),r;r.value=[];let o=[],s=jr(n,`optin`),c=jr(n,`optout`);if(!t.rest){if(a.length<s)return r.issues.push({code:`too_small`,minimum:s,inclusive:!0,input:a,inst:e,origin:`array`}),r;a.length>n.length&&r.issues.push({code:`too_big`,maximum:n.length,inclusive:!0,input:a,inst:e,origin:`array`})}let l=Array(n.length);for(let e=0;e<n.length;e++){let t=n[e]._zod.run({value:a[e],issues:[]},i);t instanceof Promise?o.push(t.then(t=>{l[e]=t})):l[e]=t}if(t.rest){let e=n.length-1,s=a.slice(n.length);for(let n of s){e++;let a=t.rest._zod.run({value:n,issues:[]},i);a instanceof Promise?o.push(a.then(t=>Mr(t,r,e))):Mr(a,r,e)}}return o.length?Promise.all(o).then(()=>Nr(l,r,n,a,c)):Nr(l,r,n,a,c)}});function jr(e,t){for(let n=e.length-1;n>=0;n--)if(e[n]._zod[t]!==`optional`)return n+1;return 0}function Mr(e,t,n){e.issues.length&&t.issues.push(...gt(n,e.issues)),t.value[n]=e.value}function Nr(e,t,n,r,i){for(let a=0;a<n.length;a++){let n=e[a],o=a<r.length;if(n.issues.length){if(!o&&a>=i){t.value.length=a;break}t.issues.push(...gt(a,n.issues))}t.value[a]=n.value}for(let e=t.value.length-1;e>=r.length&&n[e]._zod.optout===`optional`&&t.value[e]===void 0;e--)t.value.length=e;return t}var Pr=M(`$ZodRecord`,(e,t)=>{F.init(e,t),e._zod.parse=(n,r)=>{let i=n.value;if(!et(i))return n.issues.push({expected:`record`,code:`invalid_type`,input:i,inst:e}),n;let a=[],o=t.keyType._zod.values;if(o){n.value={};let s=new Set;for(let c of o)if(typeof c==`string`||typeof c==`number`||typeof c==`symbol`){s.add(typeof c==`number`?c.toString():c);let o=t.keyType._zod.run({value:c,issues:[]},r);if(o instanceof Promise)throw Error(`Async schemas not supported in object keys currently`);if(o.issues.length){n.issues.push({code:`invalid_key`,origin:`record`,issues:o.issues.map(e=>vt(e,r,ze())),input:c,path:[c],inst:e});continue}let l=o.value,u=t.valueType._zod.run({value:i[c],issues:[]},r);u instanceof Promise?a.push(u.then(e=>{e.issues.length&&n.issues.push(...gt(c,e.issues)),n.value[l]=e.value})):(u.issues.length&&n.issues.push(...gt(c,u.issues)),n.value[l]=u.value)}let c;for(let e in i)s.has(e)||(c??=[],c.push(e));c&&c.length>0&&n.issues.push({code:`unrecognized_keys`,input:i,inst:e,keys:c})}else{n.value={};for(let o of Reflect.ownKeys(i)){if(o===`__proto__`||!Object.prototype.propertyIsEnumerable.call(i,o))continue;let s=t.keyType._zod.run({value:o,issues:[]},r);if(s instanceof Promise)throw Error(`Async schemas not supported in object keys currently`);if(typeof o==`string`&&mn.test(o)&&s.issues.length){let e=t.keyType._zod.run({value:Number(o),issues:[]},r);if(e instanceof Promise)throw Error(`Async schemas not supported in object keys currently`);e.issues.length===0&&(s=e)}if(s.issues.length){t.mode===`loose`?n.value[o]=i[o]:n.issues.push({code:`invalid_key`,origin:`record`,issues:s.issues.map(e=>vt(e,r,ze())),input:o,path:[o],inst:e});continue}let c=t.valueType._zod.run({value:i[o],issues:[]},r);c instanceof Promise?a.push(c.then(e=>{e.issues.length&&n.issues.push(...gt(o,e.issues)),n.value[s.value]=e.value})):(c.issues.length&&n.issues.push(...gt(o,c.issues)),n.value[s.value]=c.value)}}return a.length?Promise.all(a).then(()=>n):n}}),Fr=M(`$ZodEnum`,(e,t)=>{F.init(e,t);let n=Be(t.entries),r=new Set(n);e._zod.values=r,e._zod.pattern=RegExp(`^(${n.filter(e=>nt.has(typeof e)).map(e=>typeof e==`string`?rt(e):e.toString()).join(`|`)})$`),e._zod.parse=(t,i)=>{let a=t.value;return r.has(a)||t.issues.push({code:`invalid_value`,values:n,input:a,inst:e}),t}}),Ir=M(`$ZodLiteral`,(e,t)=>{if(F.init(e,t),t.values.length===0)throw Error(`Cannot create literal schema with no valid values`);let n=new Set(t.values);e._zod.values=n,e._zod.pattern=RegExp(`^(${t.values.map(e=>typeof e==`string`?rt(e):e?rt(e.toString()):String(e)).join(`|`)})$`),e._zod.parse=(r,i)=>{let a=r.value;return n.has(a)||r.issues.push({code:`invalid_value`,values:t.values,input:a,inst:e}),r}}),Lr=M(`$ZodTransform`,(e,t)=>{F.init(e,t),e._zod.optin=`optional`,e._zod.parse=(n,r)=>{if(r.direction===`backward`)throw new Le(e.constructor.name);let i=t.transform(n.value,n);if(r.async)return(i instanceof Promise?i:Promise.resolve(i)).then(e=>(n.value=e,n.fallback=!0,n));if(i instanceof Promise)throw new Ie;return n.value=i,n.fallback=!0,n}});function Rr(e,t){return t===void 0&&(e.issues.length||e.fallback)?{issues:[],value:void 0}:e}var zr=M(`$ZodOptional`,(e,t)=>{F.init(e,t),e._zod.optin=`optional`,e._zod.optout=`optional`,N(e._zod,`values`,()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,void 0]):void 0),N(e._zod,`pattern`,()=>{let e=t.innerType._zod.pattern;return e?RegExp(`^(${We(e.source)})?$`):void 0}),e._zod.parse=(e,n)=>{if(t.innerType._zod.optin===`optional`){let r=e.value,i=t.innerType._zod.run(e,n);return i instanceof Promise?i.then(e=>Rr(e,r)):Rr(i,r)}return e.value===void 0?e:t.innerType._zod.run(e,n)}}),Br=M(`$ZodExactOptional`,(e,t)=>{zr.init(e,t),N(e._zod,`values`,()=>t.innerType._zod.values),N(e._zod,`pattern`,()=>t.innerType._zod.pattern),e._zod.parse=(e,n)=>t.innerType._zod.run(e,n)}),Vr=M(`$ZodNullable`,(e,t)=>{F.init(e,t),N(e._zod,`optin`,()=>t.innerType._zod.optin),N(e._zod,`optout`,()=>t.innerType._zod.optout),N(e._zod,`pattern`,()=>{let e=t.innerType._zod.pattern;return e?RegExp(`^(${We(e.source)}|null)$`):void 0}),N(e._zod,`values`,()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,null]):void 0),e._zod.parse=(e,n)=>e.value===null?e:t.innerType._zod.run(e,n)}),Hr=M(`$ZodDefault`,(e,t)=>{F.init(e,t),e._zod.optin=`optional`,N(e._zod,`values`,()=>t.innerType._zod.values),e._zod.parse=(e,n)=>{if(n.direction===`backward`)return t.innerType._zod.run(e,n);if(e.value===void 0)return e.value=t.defaultValue,e;let r=t.innerType._zod.run(e,n);return r instanceof Promise?r.then(e=>Ur(e,t)):Ur(r,t)}});function Ur(e,t){return e.value===void 0&&(e.value=t.defaultValue),e}var Wr=M(`$ZodPrefault`,(e,t)=>{F.init(e,t),e._zod.optin=`optional`,N(e._zod,`values`,()=>t.innerType._zod.values),e._zod.parse=(e,n)=>(n.direction===`backward`||e.value===void 0&&(e.value=t.defaultValue),t.innerType._zod.run(e,n))}),Gr=M(`$ZodNonOptional`,(e,t)=>{F.init(e,t),N(e._zod,`values`,()=>{let e=t.innerType._zod.values;return e?new Set([...e].filter(e=>e!==void 0)):void 0}),e._zod.parse=(n,r)=>{let i=t.innerType._zod.run(n,r);return i instanceof Promise?i.then(t=>Kr(t,e)):Kr(i,e)}});function Kr(e,t){return!e.issues.length&&e.value===void 0&&e.issues.push({code:`invalid_type`,expected:`nonoptional`,input:e.value,inst:t}),e}var qr=M(`$ZodCatch`,(e,t)=>{F.init(e,t),e._zod.optin=`optional`,N(e._zod,`optout`,()=>t.innerType._zod.optout),N(e._zod,`values`,()=>t.innerType._zod.values),e._zod.parse=(e,n)=>{if(n.direction===`backward`)return t.innerType._zod.run(e,n);let r=t.innerType._zod.run(e,n);return r instanceof Promise?r.then(r=>(e.value=r.value,r.issues.length&&(e.value=t.catchValue({...e,error:{issues:r.issues.map(e=>vt(e,n,ze()))},input:e.value}),e.issues=[],e.fallback=!0),e)):(e.value=r.value,r.issues.length&&(e.value=t.catchValue({...e,error:{issues:r.issues.map(e=>vt(e,n,ze()))},input:e.value}),e.issues=[],e.fallback=!0),e)}}),Jr=M(`$ZodPipe`,(e,t)=>{F.init(e,t),N(e._zod,`values`,()=>t.in._zod.values),N(e._zod,`optin`,()=>t.in._zod.optin),N(e._zod,`optout`,()=>t.out._zod.optout),N(e._zod,`propValues`,()=>t.in._zod.propValues),e._zod.parse=(e,n)=>{if(n.direction===`backward`){let r=t.out._zod.run(e,n);return r instanceof Promise?r.then(e=>Yr(e,t.in,n)):Yr(r,t.in,n)}let r=t.in._zod.run(e,n);return r instanceof Promise?r.then(e=>Yr(e,t.out,n)):Yr(r,t.out,n)}});function Yr(e,t,n){return e.issues.length?(e.aborted=!0,e):t._zod.run({value:e.value,issues:e.issues,fallback:e.fallback},n)}var Xr=M(`$ZodReadonly`,(e,t)=>{F.init(e,t),N(e._zod,`propValues`,()=>t.innerType._zod.propValues),N(e._zod,`values`,()=>t.innerType._zod.values),N(e._zod,`optin`,()=>t.innerType?._zod?.optin),N(e._zod,`optout`,()=>t.innerType?._zod?.optout),e._zod.parse=(e,n)=>{if(n.direction===`backward`)return t.innerType._zod.run(e,n);let r=t.innerType._zod.run(e,n);return r instanceof Promise?r.then(Zr):Zr(r)}});function Zr(e){return e.value=Object.freeze(e.value),e}var Qr=M(`$ZodLazy`,(e,t)=>{F.init(e,t),N(e._zod,`innerType`,()=>{let e=t;return e._cachedInner||=t.getter(),e._cachedInner}),N(e._zod,`pattern`,()=>e._zod.innerType?._zod?.pattern),N(e._zod,`propValues`,()=>e._zod.innerType?._zod?.propValues),N(e._zod,`optin`,()=>e._zod.innerType?._zod?.optin??void 0),N(e._zod,`optout`,()=>e._zod.innerType?._zod?.optout??void 0),e._zod.parse=(t,n)=>e._zod.innerType._zod.run(t,n)}),$r=M(`$ZodCustom`,(e,t)=>{yn.init(e,t),F.init(e,t),e._zod.parse=(e,t)=>e,e._zod.check=n=>{let r=n.value,i=t.fn(r);if(i instanceof Promise)return i.then(t=>ei(t,n,r,e));ei(i,n,r,e)}});function ei(e,t,n,r){if(!e){let e={code:`custom`,input:n,inst:r,path:[...r._zod.def.path??[]],continue:!r._zod.def.abort};r._zod.def.params&&(e.params=r._zod.def.params),t.issues.push(bt(e))}}var ti,ni=class{constructor(){this._map=new WeakMap,this._idmap=new Map}add(e,...t){let n=t[0];return this._map.set(e,n),n&&typeof n==`object`&&`id`in n&&this._idmap.set(n.id,e),this}clear(){return this._map=new WeakMap,this._idmap=new Map,this}remove(e){let t=this._map.get(e);return t&&typeof t==`object`&&`id`in t&&this._idmap.delete(t.id),this._map.delete(e),this}get(e){let t=e._zod.parent;if(t){let n={...this.get(t)??{}};delete n.id;let r={...n,...this._map.get(e)};return Object.keys(r).length?r:void 0}return this._map.get(e)}has(e){return this._map.has(e)}};function ri(){return new ni}(ti=globalThis).__zod_globalRegistry??(ti.__zod_globalRegistry=ri());var ii=globalThis.__zod_globalRegistry;function ai(e,t){return new e({type:`string`,...P(t)})}function oi(e,t){return new e({type:`string`,format:`email`,check:`string_format`,abort:!1,...P(t)})}function si(e,t){return new e({type:`string`,format:`guid`,check:`string_format`,abort:!1,...P(t)})}function ci(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,...P(t)})}function li(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,version:`v4`,...P(t)})}function ui(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,version:`v6`,...P(t)})}function di(e,t){return new e({type:`string`,format:`uuid`,check:`string_format`,abort:!1,version:`v7`,...P(t)})}function fi(e,t){return new e({type:`string`,format:`url`,check:`string_format`,abort:!1,...P(t)})}function pi(e,t){return new e({type:`string`,format:`emoji`,check:`string_format`,abort:!1,...P(t)})}function mi(e,t){return new e({type:`string`,format:`nanoid`,check:`string_format`,abort:!1,...P(t)})}function hi(e,t){return new e({type:`string`,format:`cuid`,check:`string_format`,abort:!1,...P(t)})}function gi(e,t){return new e({type:`string`,format:`cuid2`,check:`string_format`,abort:!1,...P(t)})}function _i(e,t){return new e({type:`string`,format:`ulid`,check:`string_format`,abort:!1,...P(t)})}function vi(e,t){return new e({type:`string`,format:`xid`,check:`string_format`,abort:!1,...P(t)})}function yi(e,t){return new e({type:`string`,format:`ksuid`,check:`string_format`,abort:!1,...P(t)})}function bi(e,t){return new e({type:`string`,format:`ipv4`,check:`string_format`,abort:!1,...P(t)})}function xi(e,t){return new e({type:`string`,format:`ipv6`,check:`string_format`,abort:!1,...P(t)})}function Si(e,t){return new e({type:`string`,format:`cidrv4`,check:`string_format`,abort:!1,...P(t)})}function Ci(e,t){return new e({type:`string`,format:`cidrv6`,check:`string_format`,abort:!1,...P(t)})}function wi(e,t){return new e({type:`string`,format:`base64`,check:`string_format`,abort:!1,...P(t)})}function Ti(e,t){return new e({type:`string`,format:`base64url`,check:`string_format`,abort:!1,...P(t)})}function Ei(e,t){return new e({type:`string`,format:`e164`,check:`string_format`,abort:!1,...P(t)})}function Di(e,t){return new e({type:`string`,format:`jwt`,check:`string_format`,abort:!1,...P(t)})}function Oi(e,t){return new e({type:`string`,format:`datetime`,check:`string_format`,offset:!1,local:!1,precision:null,...P(t)})}function ki(e,t){return new e({type:`string`,format:`date`,check:`string_format`,...P(t)})}function Ai(e,t){return new e({type:`string`,format:`time`,check:`string_format`,precision:null,...P(t)})}function ji(e,t){return new e({type:`string`,format:`duration`,check:`string_format`,...P(t)})}function Mi(e,t){return new e({type:`number`,checks:[],...P(t)})}function Ni(e,t){return new e({type:`number`,check:`number_format`,abort:!1,format:`safeint`,...P(t)})}function Pi(e,t){return new e({type:`boolean`,...P(t)})}function Fi(e,t){return new e({type:`null`,...P(t)})}function Ii(e){return new e({type:`unknown`})}function Li(e,t){return new e({type:`never`,...P(t)})}function Ri(e,t){return new xn({check:`less_than`,...P(t),value:e,inclusive:!1})}function zi(e,t){return new xn({check:`less_than`,...P(t),value:e,inclusive:!0})}function Bi(e,t){return new Sn({check:`greater_than`,...P(t),value:e,inclusive:!1})}function Vi(e,t){return new Sn({check:`greater_than`,...P(t),value:e,inclusive:!0})}function Hi(e,t){return new Cn({check:`multiple_of`,...P(t),value:e})}function Ui(e,t){return new Tn({check:`max_length`,...P(t),maximum:e})}function Wi(e,t){return new En({check:`min_length`,...P(t),minimum:e})}function Gi(e,t){return new Dn({check:`length_equals`,...P(t),length:e})}function Ki(e,t){return new kn({check:`string_format`,format:`regex`,...P(t),pattern:e})}function qi(e){return new An({check:`string_format`,format:`lowercase`,...P(e)})}function Ji(e){return new jn({check:`string_format`,format:`uppercase`,...P(e)})}function Yi(e,t){return new Mn({check:`string_format`,format:`includes`,...P(t),includes:e})}function Xi(e,t){return new Nn({check:`string_format`,format:`starts_with`,...P(t),prefix:e})}function Zi(e,t){return new Pn({check:`string_format`,format:`ends_with`,...P(t),suffix:e})}function Qi(e){return new Fn({check:`overwrite`,tx:e})}function $i(e){return Qi(t=>t.normalize(e))}function ea(){return Qi(e=>e.trim())}function ta(){return Qi(e=>e.toLowerCase())}function na(){return Qi(e=>e.toUpperCase())}function ra(){return Qi(e=>Xe(e))}function ia(e,t,n){return new e({type:`array`,element:t,...P(n)})}function aa(e,t,n){return new e({type:`custom`,check:`custom`,fn:t,...P(n)})}function oa(e,t){let n=sa(t=>(t.addIssue=e=>{if(typeof e==`string`)t.issues.push(bt(e,t.value,n._zod.def));else{let r=e;r.fatal&&(r.continue=!1),r.code??=`custom`,r.input??=t.value,r.inst??=n,r.continue??=!n._zod.def.abort,t.issues.push(bt(r))}},e(t.value,t)),t);return n}function sa(e,t){let n=new yn({check:`custom`,...P(t)});return n._zod.check=e,n}function ca(e){let t=e?.target??`draft-2020-12`;return t===`draft-4`&&(t=`draft-04`),t===`draft-7`&&(t=`draft-07`),{processors:e.processors??{},metadataRegistry:e?.metadata??ii,target:t,unrepresentable:e?.unrepresentable??`throw`,override:e?.override??(()=>{}),io:e?.io??`output`,counter:0,seen:new Map,cycles:e?.cycles??`ref`,reused:e?.reused??`inline`,external:e?.external??void 0}}function L(e,t,n={path:[],schemaPath:[]}){var r;let i=e._zod.def,a=t.seen.get(e);if(a)return a.count++,n.schemaPath.includes(e)&&(a.cycle=n.path),a.schema;let o={schema:{},count:1,cycle:void 0,path:n.path};t.seen.set(e,o);let s=e._zod.toJSONSchema?.();if(s)o.schema=s;else{let r={...n,schemaPath:[...n.schemaPath,e],path:n.path};if(e._zod.processJSONSchema)e._zod.processJSONSchema(t,o.schema,r);else{let n=o.schema,a=t.processors[i.type];if(!a)throw Error(`[toJSONSchema]: Non-representable type encountered: ${i.type}`);a(e,t,n,r)}let a=e._zod.parent;a&&(o.ref||=a,L(a,t,r),t.seen.get(a).isParent=!0)}let c=t.metadataRegistry.get(e);return c&&Object.assign(o.schema,c),t.io===`input`&&da(e)&&(delete o.schema.examples,delete o.schema.default),t.io===`input`&&`_prefault`in o.schema&&((r=o.schema).default??(r.default=o.schema._prefault)),delete o.schema._prefault,t.seen.get(e).schema}function la(e,t){let n=e.seen.get(t);if(!n)throw Error(`Unprocessed schema. This is a bug in Zod.`);let r=new Map;for(let t of e.seen.entries()){let n=e.metadataRegistry.get(t[0])?.id;if(n){let e=r.get(n);if(e&&e!==t[0])throw Error(`Duplicate schema id "${n}" detected during JSON Schema conversion. Two different schemas cannot share the same id when converted together.`);r.set(n,t[0])}}let i=t=>{let r=e.target===`draft-2020-12`?`$defs`:`definitions`;if(e.external){let n=e.external.registry.get(t[0])?.id,i=e.external.uri??(e=>e);if(n)return{ref:i(n)};let a=t[1].defId??t[1].schema.id??`schema${e.counter++}`;return t[1].defId=a,{defId:a,ref:`${i(`__shared`)}#/${r}/${a}`}}if(t[1]===n)return{ref:`#`};let i=`#/${r}/`,a=t[1].schema.id??`__schema${e.counter++}`;return{defId:a,ref:i+a}},a=e=>{if(e[1].schema.$ref)return;let t=e[1],{ref:n,defId:r}=i(e);t.def={...t.schema},r&&(t.defId=r);let a=t.schema;for(let e in a)delete a[e];a.$ref=n};if(e.cycles===`throw`)for(let t of e.seen.entries()){let e=t[1];if(e.cycle)throw Error(`Cycle detected: #/${e.cycle?.join(`/`)}/<root>

Set the \`cycles\` parameter to \`"ref"\` to resolve cyclical schemas with defs.`)}for(let n of e.seen.entries()){let r=n[1];if(t===n[0]){a(n);continue}if(e.external){let r=e.external.registry.get(n[0])?.id;if(t!==n[0]&&r){a(n);continue}}if(e.metadataRegistry.get(n[0])?.id){a(n);continue}if(r.cycle){a(n);continue}if(r.count>1&&e.reused===`ref`){a(n);continue}}}function ua(e,t){let n=e.seen.get(t);if(!n)throw Error(`Unprocessed schema. This is a bug in Zod.`);let r=t=>{let n=e.seen.get(t);if(n.ref===null)return;let i=n.def??n.schema,a={...i},o=n.ref;if(n.ref=null,o){r(o);let n=e.seen.get(o),s=n.schema;if(s.$ref&&(e.target===`draft-07`||e.target===`draft-04`||e.target===`openapi-3.0`)?(i.allOf=i.allOf??[],i.allOf.push(s)):Object.assign(i,s),Object.assign(i,a),t._zod.parent===o)for(let e in i)e===`$ref`||e===`allOf`||e in a||delete i[e];if(s.$ref&&n.def)for(let e in i)e===`$ref`||e===`allOf`||e in n.def&&JSON.stringify(i[e])===JSON.stringify(n.def[e])&&delete i[e]}let s=t._zod.parent;if(s&&s!==o){r(s);let t=e.seen.get(s);if(t?.schema.$ref&&(i.$ref=t.schema.$ref,t.def))for(let e in i)e===`$ref`||e===`allOf`||e in t.def&&JSON.stringify(i[e])===JSON.stringify(t.def[e])&&delete i[e]}e.override({zodSchema:t,jsonSchema:i,path:n.path??[]})};for(let t of[...e.seen.entries()].reverse())r(t[0]);let i={};if(e.target===`draft-2020-12`?i.$schema=`https://json-schema.org/draft/2020-12/schema`:e.target===`draft-07`?i.$schema=`http://json-schema.org/draft-07/schema#`:e.target===`draft-04`?i.$schema=`http://json-schema.org/draft-04/schema#`:e.target,e.external?.uri){let n=e.external.registry.get(t)?.id;if(!n)throw Error("Schema is missing an `id` property");i.$id=e.external.uri(n)}Object.assign(i,n.def??n.schema);let a=e.metadataRegistry.get(t)?.id;a!==void 0&&i.id===a&&delete i.id;let o=e.external?.defs??{};for(let t of e.seen.entries()){let e=t[1];e.def&&e.defId&&(e.def.id===e.defId&&delete e.def.id,o[e.defId]=e.def)}e.external||Object.keys(o).length>0&&(e.target===`draft-2020-12`?i.$defs=o:i.definitions=o);try{let n=JSON.parse(JSON.stringify(i));return Object.defineProperty(n,"~standard",{value:{...t[`~standard`],jsonSchema:{input:pa(t,`input`,e.processors),output:pa(t,`output`,e.processors)}},enumerable:!1,writable:!1}),n}catch{throw Error(`Error converting schema to JSON.`)}}function da(e,t){let n=t??{seen:new Set};if(n.seen.has(e))return!1;n.seen.add(e);let r=e._zod.def;if(r.type===`transform`)return!0;if(r.type===`array`)return da(r.element,n);if(r.type===`set`)return da(r.valueType,n);if(r.type===`lazy`)return da(r.getter(),n);if(r.type===`promise`||r.type===`optional`||r.type===`nonoptional`||r.type===`nullable`||r.type===`readonly`||r.type==="default"||r.type===`prefault`)return da(r.innerType,n);if(r.type===`intersection`)return da(r.left,n)||da(r.right,n);if(r.type===`record`||r.type===`map`)return da(r.keyType,n)||da(r.valueType,n);if(r.type===`pipe`)return e._zod.traits.has(`$ZodCodec`)?!0:da(r.in,n)||da(r.out,n);if(r.type===`object`){for(let e in r.shape)if(da(r.shape[e],n))return!0;return!1}if(r.type===`union`){for(let e of r.options)if(da(e,n))return!0;return!1}if(r.type===`tuple`){for(let e of r.items)if(da(e,n))return!0;return!!(r.rest&&da(r.rest,n))}return!1}var fa=(e,t={})=>n=>{let r=ca({...n,processors:t});return L(e,r),la(r,e),ua(r,e)},pa=(e,t,n={})=>r=>{let{libraryOptions:i,target:a}=r??{},o=ca({...i??{},target:a,io:t,processors:n});return L(e,o),la(o,e),ua(o,e)},ma={guid:`uuid`,url:`uri`,datetime:`date-time`,json_string:`json-string`,regex:``},ha=(e,t,n,r)=>{let i=n;i.type=`string`;let{minimum:a,maximum:o,format:s,patterns:c,contentEncoding:l}=e._zod.bag;if(typeof a==`number`&&(i.minLength=a),typeof o==`number`&&(i.maxLength=o),s&&(i.format=ma[s]??s,i.format===``&&delete i.format,s===`time`&&delete i.format),l&&(i.contentEncoding=l),c&&c.size>0){let e=[...c];e.length===1?i.pattern=e[0].source:e.length>1&&(i.allOf=[...e.map(e=>({...t.target===`draft-07`||t.target===`draft-04`||t.target===`openapi-3.0`?{type:`string`}:{},pattern:e.source}))])}},ga=(e,t,n,r)=>{let i=n,{minimum:a,maximum:o,format:s,multipleOf:c,exclusiveMaximum:l,exclusiveMinimum:u}=e._zod.bag;typeof s==`string`&&s.includes(`int`)?i.type=`integer`:i.type=`number`;let d=typeof u==`number`&&u>=(a??-1/0),f=typeof l==`number`&&l<=(o??1/0),p=t.target===`draft-04`||t.target===`openapi-3.0`;d?p?(i.minimum=u,i.exclusiveMinimum=!0):i.exclusiveMinimum=u:typeof a==`number`&&(i.minimum=a),f?p?(i.maximum=l,i.exclusiveMaximum=!0):i.exclusiveMaximum=l:typeof o==`number`&&(i.maximum=o),typeof c==`number`&&(i.multipleOf=c)},_a=(e,t,n,r)=>{n.type=`boolean`},va=(e,t,n,r)=>{t.target===`openapi-3.0`?(n.type=`string`,n.nullable=!0,n.enum=[null]):n.type=`null`},ya=(e,t,n,r)=>{n.not={}},ba=(e,t,n,r)=>{let i=e._zod.def,a=Be(i.entries);a.every(e=>typeof e==`number`)&&(n.type=`number`),a.every(e=>typeof e==`string`)&&(n.type=`string`),n.enum=a},xa=(e,t,n,r)=>{let i=e._zod.def,a=[];for(let e of i.values)if(e===void 0){if(t.unrepresentable===`throw`)throw Error("Literal `undefined` cannot be represented in JSON Schema")}else if(typeof e==`bigint`){if(t.unrepresentable===`throw`)throw Error(`BigInt literals cannot be represented in JSON Schema`);a.push(Number(e))}else a.push(e);if(a.length!==0)if(a.length===1){let e=a[0];n.type=e===null?`null`:typeof e,t.target===`draft-04`||t.target===`openapi-3.0`?n.enum=[e]:n.const=e}else a.every(e=>typeof e==`number`)&&(n.type=`number`),a.every(e=>typeof e==`string`)&&(n.type=`string`),a.every(e=>typeof e==`boolean`)&&(n.type=`boolean`),a.every(e=>e===null)&&(n.type=`null`),n.enum=a},Sa=(e,t,n,r)=>{if(t.unrepresentable===`throw`)throw Error(`Custom types cannot be represented in JSON Schema`)},Ca=(e,t,n,r)=>{if(t.unrepresentable===`throw`)throw Error(`Transforms cannot be represented in JSON Schema`)},wa=(e,t,n,r)=>{let i=n,a=e._zod.def,{minimum:o,maximum:s}=e._zod.bag;typeof o==`number`&&(i.minItems=o),typeof s==`number`&&(i.maxItems=s),i.type=`array`,i.items=L(a.element,t,{...r,path:[...r.path,`items`]})},Ta=(e,t,n,r)=>{let i=n,a=e._zod.def;i.type=`object`,i.properties={};let o=a.shape;for(let e in o)i.properties[e]=L(o[e],t,{...r,path:[...r.path,`properties`,e]});let s=new Set(Object.keys(o)),c=new Set([...s].filter(e=>{let n=a.shape[e]._zod;return t.io===`input`?n.optin===void 0:n.optout===void 0}));c.size>0&&(i.required=Array.from(c)),a.catchall?._zod.def.type===`never`?i.additionalProperties=!1:a.catchall?a.catchall&&(i.additionalProperties=L(a.catchall,t,{...r,path:[...r.path,`additionalProperties`]})):t.io===`output`&&(i.additionalProperties=!1)},Ea=(e,t,n,r)=>{let i=e._zod.def,a=i.inclusive===!1,o=i.options.map((e,n)=>L(e,t,{...r,path:[...r.path,a?`oneOf`:`anyOf`,n]}));a?n.oneOf=o:n.anyOf=o},Da=(e,t,n,r)=>{let i=e._zod.def,a=L(i.left,t,{...r,path:[...r.path,`allOf`,0]}),o=L(i.right,t,{...r,path:[...r.path,`allOf`,1]}),s=e=>`allOf`in e&&Object.keys(e).length===1;n.allOf=[...s(a)?a.allOf:[a],...s(o)?o.allOf:[o]]},Oa=(e,t,n,r)=>{let i=n,a=e._zod.def;i.type=`array`;let o=t.target===`draft-2020-12`?`prefixItems`:`items`,s=t.target===`draft-2020-12`||t.target===`openapi-3.0`?`items`:`additionalItems`,c=a.items.map((e,n)=>L(e,t,{...r,path:[...r.path,o,n]})),l=a.rest?L(a.rest,t,{...r,path:[...r.path,s,...t.target===`openapi-3.0`?[a.items.length]:[]]}):null;t.target===`draft-2020-12`?(i.prefixItems=c,l&&(i.items=l)):t.target===`openapi-3.0`?(i.items={anyOf:c},l&&i.items.anyOf.push(l),i.minItems=c.length,l||(i.maxItems=c.length)):(i.items=c,l&&(i.additionalItems=l));let{minimum:u,maximum:d}=e._zod.bag;typeof u==`number`&&(i.minItems=u),typeof d==`number`&&(i.maxItems=d)},ka=(e,t,n,r)=>{let i=n,a=e._zod.def;i.type=`object`;let o=a.keyType,s=o._zod.bag?.patterns;if(a.mode===`loose`&&s&&s.size>0){let e=L(a.valueType,t,{...r,path:[...r.path,`patternProperties`,`*`]});i.patternProperties={};for(let t of s)i.patternProperties[t.source]=e}else(t.target===`draft-07`||t.target===`draft-2020-12`)&&(i.propertyNames=L(a.keyType,t,{...r,path:[...r.path,`propertyNames`]})),i.additionalProperties=L(a.valueType,t,{...r,path:[...r.path,`additionalProperties`]});let c=o._zod.values;if(c){let e=[...c].filter(e=>typeof e==`string`||typeof e==`number`);e.length>0&&(i.required=e)}},Aa=(e,t,n,r)=>{let i=e._zod.def,a=L(i.innerType,t,r),o=t.seen.get(e);t.target===`openapi-3.0`?(o.ref=i.innerType,n.nullable=!0):n.anyOf=[a,{type:`null`}]},ja=(e,t,n,r)=>{let i=e._zod.def;L(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType},Ma=(e,t,n,r)=>{let i=e._zod.def;L(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType,n.default=JSON.parse(JSON.stringify(i.defaultValue))},Na=(e,t,n,r)=>{let i=e._zod.def;L(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType,t.io===`input`&&(n._prefault=JSON.parse(JSON.stringify(i.defaultValue)))},Pa=(e,t,n,r)=>{let i=e._zod.def;L(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType;let o;try{o=i.catchValue(void 0)}catch{throw Error(`Dynamic catch values are not supported in JSON Schema`)}n.default=o},Fa=(e,t,n,r)=>{let i=e._zod.def,a=i.in._zod.traits.has(`$ZodTransform`),o=t.io===`input`?a?i.out:i.in:i.out;L(o,t,r);let s=t.seen.get(e);s.ref=o},Ia=(e,t,n,r)=>{let i=e._zod.def;L(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType,n.readOnly=!0},La=(e,t,n,r)=>{let i=e._zod.def;L(i.innerType,t,r);let a=t.seen.get(e);a.ref=i.innerType},Ra=(e,t,n,r)=>{let i=e._zod.innerType;L(i,t,r);let a=t.seen.get(e);a.ref=i},za=M(`ZodISODateTime`,(e,t)=>{Xn.init(e,t),B.init(e,t)});function Ba(e){return Oi(za,e)}var Va=M(`ZodISODate`,(e,t)=>{Zn.init(e,t),B.init(e,t)});function Ha(e){return ki(Va,e)}var Ua=M(`ZodISOTime`,(e,t)=>{Qn.init(e,t),B.init(e,t)});function Wa(e){return Ai(Ua,e)}var Ga=M(`ZodISODuration`,(e,t)=>{$n.init(e,t),B.init(e,t)});function Ka(e){return ji(Ga,e)}var qa=M(`ZodError`,(e,t)=>{St.init(e,t),e.name=`ZodError`,Object.defineProperties(e,{format:{value:t=>Tt(e,t)},flatten:{value:t=>wt(e,t)},addIssue:{value:t=>{e.issues.push(t),e.message=JSON.stringify(e.issues,Ve,2)}},addIssues:{value:t=>{e.issues.push(...t),e.message=JSON.stringify(e.issues,Ve,2)}},isEmpty:{get(){return e.issues.length===0}}})},{Parent:Error}),Ja=Et(qa),Ya=Dt(qa),Xa=Ot(qa),Za=At(qa),Qa=Mt(qa),$a=Nt(qa),eo=Pt(qa),to=Ft(qa),no=It(qa),ro=Lt(qa),io=Rt(qa),ao=zt(qa),oo=new WeakMap;function so(e,t,n){let r=Object.getPrototypeOf(e),i=oo.get(r);if(i||(i=new Set,oo.set(r,i)),!i.has(t)){i.add(t);for(let e in n){let t=n[e];Object.defineProperty(r,e,{configurable:!0,enumerable:!1,get(){let n=t.bind(this);return Object.defineProperty(this,e,{configurable:!0,writable:!0,enumerable:!0,value:n}),n},set(t){Object.defineProperty(this,e,{configurable:!0,writable:!0,enumerable:!0,value:t})}})}}}var R=M(`ZodType`,(e,t)=>(F.init(e,t),Object.assign(e[`~standard`],{jsonSchema:{input:pa(e,`input`),output:pa(e,`output`)}}),e.toJSONSchema=fa(e,{}),e.def=t,e.type=t.type,Object.defineProperty(e,"_def",{value:t}),e.parse=(t,n)=>Ja(e,t,n,{callee:e.parse}),e.safeParse=(t,n)=>Xa(e,t,n),e.parseAsync=async(t,n)=>Ya(e,t,n,{callee:e.parseAsync}),e.safeParseAsync=async(t,n)=>Za(e,t,n),e.spa=e.safeParseAsync,e.encode=(t,n)=>Qa(e,t,n),e.decode=(t,n)=>$a(e,t,n),e.encodeAsync=async(t,n)=>eo(e,t,n),e.decodeAsync=async(t,n)=>to(e,t,n),e.safeEncode=(t,n)=>no(e,t,n),e.safeDecode=(t,n)=>ro(e,t,n),e.safeEncodeAsync=async(t,n)=>io(e,t,n),e.safeDecodeAsync=async(t,n)=>ao(e,t,n),so(e,`ZodType`,{check(...e){let t=this.def;return this.clone(Je(t,{checks:[...t.checks??[],...e.map(e=>typeof e==`function`?{_zod:{check:e,def:{check:`custom`},onattach:[]}}:e)]}),{parent:!0})},with(...e){return this.check(...e)},clone(e,t){return it(this,e,t)},brand(){return this},register(e,t){return e.add(this,t),this},refine(e,t){return this.check(Es(e,t))},superRefine(e,t){return this.check(Ds(e,t))},overwrite(e){return this.check(Qi(e))},optional(){return os(this)},exactOptional(){return cs(this)},nullable(){return us(this)},nullish(){return os(us(this))},nonoptional(e){return gs(this,e)},array(){return H(this)},or(e){return Wo([this,e])},and(e){return Jo(this,e)},transform(e){return bs(this,is(e))},default(e){return fs(this,e)},prefault(e){return ms(this,e)},catch(e){return vs(this,e)},pipe(e){return bs(this,e)},readonly(){return Ss(this)},describe(e){let t=this.clone();return ii.add(t,{description:e}),t},meta(...e){if(e.length===0)return ii.get(this);let t=this.clone();return ii.add(t,e[0]),t},isOptional(){return this.safeParse(void 0).success},isNullable(){return this.safeParse(null).success},apply(e){return e(this)}}),Object.defineProperty(e,"description",{get(){return ii.get(e)?.description},configurable:!0}),e)),co=M(`_ZodString`,(e,t)=>{Rn.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ha(e,t,n,r);let n=e._zod.bag;e.format=n.format??null,e.minLength=n.minimum??null,e.maxLength=n.maximum??null,so(e,`_ZodString`,{regex(...e){return this.check(Ki(...e))},includes(...e){return this.check(Yi(...e))},startsWith(...e){return this.check(Xi(...e))},endsWith(...e){return this.check(Zi(...e))},min(...e){return this.check(Wi(...e))},max(...e){return this.check(Ui(...e))},length(...e){return this.check(Gi(...e))},nonempty(...e){return this.check(Wi(1,...e))},lowercase(e){return this.check(qi(e))},uppercase(e){return this.check(Ji(e))},trim(){return this.check(ea())},normalize(...e){return this.check($i(...e))},toLowerCase(){return this.check(ta())},toUpperCase(){return this.check(na())},slugify(){return this.check(ra())}})}),lo=M(`ZodString`,(e,t)=>{Rn.init(e,t),co.init(e,t),e.email=t=>e.check(oi(uo,t)),e.url=t=>e.check(fi(mo,t)),e.jwt=t=>e.check(Di(ko,t)),e.emoji=t=>e.check(pi(ho,t)),e.guid=t=>e.check(si(fo,t)),e.uuid=t=>e.check(ci(po,t)),e.uuidv4=t=>e.check(li(po,t)),e.uuidv6=t=>e.check(ui(po,t)),e.uuidv7=t=>e.check(di(po,t)),e.nanoid=t=>e.check(mi(go,t)),e.guid=t=>e.check(si(fo,t)),e.cuid=t=>e.check(hi(_o,t)),e.cuid2=t=>e.check(gi(vo,t)),e.ulid=t=>e.check(_i(yo,t)),e.base64=t=>e.check(wi(Eo,t)),e.base64url=t=>e.check(Ti(Do,t)),e.xid=t=>e.check(vi(bo,t)),e.ksuid=t=>e.check(yi(xo,t)),e.ipv4=t=>e.check(bi(So,t)),e.ipv6=t=>e.check(xi(Co,t)),e.cidrv4=t=>e.check(Si(wo,t)),e.cidrv6=t=>e.check(Ci(To,t)),e.e164=t=>e.check(Ei(Oo,t)),e.datetime=t=>e.check(Ba(t)),e.date=t=>e.check(Ha(t)),e.time=t=>e.check(Wa(t)),e.duration=t=>e.check(Ka(t))});function z(e){return ai(lo,e)}var B=M(`ZodStringFormat`,(e,t)=>{I.init(e,t),co.init(e,t)}),uo=M(`ZodEmail`,(e,t)=>{Vn.init(e,t),B.init(e,t)}),fo=M(`ZodGUID`,(e,t)=>{zn.init(e,t),B.init(e,t)}),po=M(`ZodUUID`,(e,t)=>{Bn.init(e,t),B.init(e,t)}),mo=M(`ZodURL`,(e,t)=>{Hn.init(e,t),B.init(e,t)}),ho=M(`ZodEmoji`,(e,t)=>{Un.init(e,t),B.init(e,t)}),go=M(`ZodNanoID`,(e,t)=>{Wn.init(e,t),B.init(e,t)}),_o=M(`ZodCUID`,(e,t)=>{Gn.init(e,t),B.init(e,t)}),vo=M(`ZodCUID2`,(e,t)=>{Kn.init(e,t),B.init(e,t)}),yo=M(`ZodULID`,(e,t)=>{qn.init(e,t),B.init(e,t)}),bo=M(`ZodXID`,(e,t)=>{Jn.init(e,t),B.init(e,t)}),xo=M(`ZodKSUID`,(e,t)=>{Yn.init(e,t),B.init(e,t)}),So=M(`ZodIPv4`,(e,t)=>{er.init(e,t),B.init(e,t)}),Co=M(`ZodIPv6`,(e,t)=>{tr.init(e,t),B.init(e,t)}),wo=M(`ZodCIDRv4`,(e,t)=>{nr.init(e,t),B.init(e,t)}),To=M(`ZodCIDRv6`,(e,t)=>{rr.init(e,t),B.init(e,t)}),Eo=M(`ZodBase64`,(e,t)=>{ar.init(e,t),B.init(e,t)}),Do=M(`ZodBase64URL`,(e,t)=>{sr.init(e,t),B.init(e,t)}),Oo=M(`ZodE164`,(e,t)=>{cr.init(e,t),B.init(e,t)}),ko=M(`ZodJWT`,(e,t)=>{ur.init(e,t),B.init(e,t)}),Ao=M(`ZodNumber`,(e,t)=>{dr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ga(e,t,n,r),so(e,`ZodNumber`,{gt(e,t){return this.check(Bi(e,t))},gte(e,t){return this.check(Vi(e,t))},min(e,t){return this.check(Vi(e,t))},lt(e,t){return this.check(Ri(e,t))},lte(e,t){return this.check(zi(e,t))},max(e,t){return this.check(zi(e,t))},int(e){return this.check(No(e))},safe(e){return this.check(No(e))},positive(e){return this.check(Bi(0,e))},nonnegative(e){return this.check(Vi(0,e))},negative(e){return this.check(Ri(0,e))},nonpositive(e){return this.check(zi(0,e))},multipleOf(e,t){return this.check(Hi(e,t))},step(e,t){return this.check(Hi(e,t))},finite(){return this}});let n=e._zod.bag;e.minValue=Math.max(n.minimum??-1/0,n.exclusiveMinimum??-1/0)??null,e.maxValue=Math.min(n.maximum??1/0,n.exclusiveMaximum??1/0)??null,e.isInt=(n.format??``).includes(`int`)||Number.isSafeInteger(n.multipleOf??.5),e.isFinite=!0,e.format=n.format??null});function jo(e){return Mi(Ao,e)}var Mo=M(`ZodNumberFormat`,(e,t)=>{fr.init(e,t),Ao.init(e,t)});function No(e){return Ni(Mo,e)}var Po=M(`ZodBoolean`,(e,t)=>{pr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>_a(e,t,n,r)});function V(e){return Pi(Po,e)}var Fo=M(`ZodNull`,(e,t)=>{mr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>va(e,t,n,r)});function Io(e){return Fi(Fo,e)}var Lo=M(`ZodUnknown`,(e,t)=>{hr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(e,t,n)=>void 0});function Ro(){return Ii(Lo)}var zo=M(`ZodNever`,(e,t)=>{gr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ya(e,t,n,r)});function Bo(e){return Li(zo,e)}var Vo=M(`ZodArray`,(e,t)=>{vr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>wa(e,t,n,r),e.element=t.element,so(e,`ZodArray`,{min(e,t){return this.check(Wi(e,t))},nonempty(e){return this.check(Wi(1,e))},max(e,t){return this.check(Ui(e,t))},length(e,t){return this.check(Gi(e,t))},unwrap(){return this.element}})});function H(e,t){return ia(Vo,e,t)}var Ho=M(`ZodObject`,(e,t)=>{Cr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Ta(e,t,n,r),N(e,`shape`,()=>t.shape),so(e,`ZodObject`,{keyof(){return es(Object.keys(this._zod.def.shape))},catchall(e){return this.clone({...this._zod.def,catchall:e})},passthrough(){return this.clone({...this._zod.def,catchall:Ro()})},loose(){return this.clone({...this._zod.def,catchall:Ro()})},strict(){return this.clone({...this._zod.def,catchall:Bo()})},strip(){return this.clone({...this._zod.def,catchall:void 0})},extend(e){return lt(this,e)},safeExtend(e){return ut(this,e)},merge(e){return dt(this,e)},pick(e){return st(this,e)},omit(e){return ct(this,e)},partial(...e){return ft(as,this,e[0])},required(...e){return pt(hs,this,e[0])}})});function U(e,t){return new Ho({type:`object`,shape:e??{},...P(t)})}var Uo=M(`ZodUnion`,(e,t)=>{Tr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Ea(e,t,n,r),e.options=t.options});function Wo(e,t){return new Uo({type:`union`,options:e,...P(t)})}var Go=M(`ZodDiscriminatedUnion`,(e,t)=>{Uo.init(e,t),Er.init(e,t)});function Ko(e,t,n){return new Go({type:`union`,options:t,discriminator:e,...P(n)})}var qo=M(`ZodIntersection`,(e,t)=>{Dr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Da(e,t,n,r)});function Jo(e,t){return new qo({type:`intersection`,left:e,right:t})}var Yo=M(`ZodTuple`,(e,t)=>{Ar.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Oa(e,t,n,r),e.rest=t=>e.clone({...e._zod.def,rest:t})});function Xo(e,t,n){let r=t instanceof F;return new Yo({type:`tuple`,items:e,rest:r?t:null,...P(r?n:t)})}var Zo=M(`ZodRecord`,(e,t)=>{Pr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ka(e,t,n,r),e.keyType=t.keyType,e.valueType=t.valueType});function Qo(e,t,n){return!t||!t._zod?new Zo({type:`record`,keyType:z(),valueType:e,...P(t)}):new Zo({type:`record`,keyType:e,valueType:t,...P(n)})}var $o=M(`ZodEnum`,(e,t)=>{Fr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ba(e,t,n,r),e.enum=t.entries,e.options=Object.values(t.entries);let n=new Set(Object.keys(t.entries));e.extract=(e,r)=>{let i={};for(let r of e)if(n.has(r))i[r]=t.entries[r];else throw Error(`Key ${r} not found in enum`);return new $o({...t,checks:[],...P(r),entries:i})},e.exclude=(e,r)=>{let i={...t.entries};for(let t of e)if(n.has(t))delete i[t];else throw Error(`Key ${t} not found in enum`);return new $o({...t,checks:[],...P(r),entries:i})}});function es(e,t){return new $o({type:`enum`,entries:Array.isArray(e)?Object.fromEntries(e.map(e=>[e,e])):e,...P(t)})}var ts=M(`ZodLiteral`,(e,t)=>{Ir.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>xa(e,t,n,r),e.values=new Set(t.values),Object.defineProperty(e,"value",{get(){if(t.values.length>1)throw Error("This schema contains multiple valid literal values. Use `.values` instead.");return t.values[0]}})});function ns(e,t){return new ts({type:`literal`,values:Array.isArray(e)?e:[e],...P(t)})}var rs=M(`ZodTransform`,(e,t)=>{Lr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Ca(e,t,n,r),e._zod.parse=(n,r)=>{if(r.direction===`backward`)throw new Le(e.constructor.name);n.addIssue=r=>{if(typeof r==`string`)n.issues.push(bt(r,n.value,t));else{let t=r;t.fatal&&(t.continue=!1),t.code??=`custom`,t.input??=n.value,t.inst??=e,n.issues.push(bt(t))}};let i=t.transform(n.value,n);return i instanceof Promise?i.then(e=>(n.value=e,n.fallback=!0,n)):(n.value=i,n.fallback=!0,n)}});function is(e){return new rs({type:`transform`,transform:e})}var as=M(`ZodOptional`,(e,t)=>{zr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>La(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function os(e){return new as({type:`optional`,innerType:e})}var ss=M(`ZodExactOptional`,(e,t)=>{Br.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>La(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function cs(e){return new ss({type:`optional`,innerType:e})}var ls=M(`ZodNullable`,(e,t)=>{Vr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Aa(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function us(e){return new ls({type:`nullable`,innerType:e})}var ds=M(`ZodDefault`,(e,t)=>{Hr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Ma(e,t,n,r),e.unwrap=()=>e._zod.def.innerType,e.removeDefault=e.unwrap});function fs(e,t){return new ds({type:`default`,innerType:e,get defaultValue(){return typeof t==`function`?t():tt(t)}})}var ps=M(`ZodPrefault`,(e,t)=>{Wr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Na(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function ms(e,t){return new ps({type:`prefault`,innerType:e,get defaultValue(){return typeof t==`function`?t():tt(t)}})}var hs=M(`ZodNonOptional`,(e,t)=>{Gr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>ja(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function gs(e,t){return new hs({type:`nonoptional`,innerType:e,...P(t)})}var _s=M(`ZodCatch`,(e,t)=>{qr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Pa(e,t,n,r),e.unwrap=()=>e._zod.def.innerType,e.removeCatch=e.unwrap});function vs(e,t){return new _s({type:`catch`,innerType:e,catchValue:typeof t==`function`?t:()=>t})}var ys=M(`ZodPipe`,(e,t)=>{Jr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Fa(e,t,n,r),e.in=t.in,e.out=t.out});function bs(e,t){return new ys({type:`pipe`,in:e,out:t})}var xs=M(`ZodReadonly`,(e,t)=>{Xr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Ia(e,t,n,r),e.unwrap=()=>e._zod.def.innerType});function Ss(e){return new xs({type:`readonly`,innerType:e})}var Cs=M(`ZodLazy`,(e,t)=>{Qr.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Ra(e,t,n,r),e.unwrap=()=>e._zod.def.getter()});function ws(e){return new Cs({type:`lazy`,getter:e})}var Ts=M(`ZodCustom`,(e,t)=>{$r.init(e,t),R.init(e,t),e._zod.processJSONSchema=(t,n,r)=>Sa(e,t,n,r)});function Es(e,t={}){return aa(Ts,e,t)}function Ds(e,t){return oa(e,t)}function Os(e){let t=ws(()=>Wo([z(e),jo(),V(),Io(),H(t),Qo(z(),t)]));return t}var W=z().uuid(),G=z().min(1),K=z().min(1),q=jo().int().nonnegative(),ks=jo().int().positive(),As=q.nullable(),js=Os(),Ms=Qo(z(),Ro()),Ns=Ms.nullable(),Ps=ns(!0),Fs=es([`queued`,`running`,`completed`,`failed`,`cancelled`]),Is=U({prompt_tokens:q,completion_tokens:q,reasoning_tokens:q.optional(),cache_creation_input_tokens:q.optional(),cache_read_input_tokens:q.optional()}).passthrough(),Ls=U({id:W,name:G,description:z(),has_telegram:V(),debug_mode:V(),is_default:V(),model:G.optional(),posture:G.optional(),provider:G.optional(),thinking_budget_tokens:q.optional(),reasoning_effort:es([`minimal`,`low`,`medium`,`high`]).optional(),gemini_thinking_budget:q.optional(),summary_provider:G.optional(),summary_model:G.optional()}).passthrough(),Rs=U({id:W,agent_id:W,context_id:z(),session_type:es([`chat`,`dm`,`notification`,`job`,`subagent`,`telegram`,`episodic`]),has_active_run:V(),participants:Xo([G,G]).optional(),agent_name:G.optional(),parent_session_id:W.nullable().optional()}).passthrough().superRefine((e,t)=>{e.session_type===`dm`&&!e.participants&&t.addIssue({code:`custom`,path:[`participants`],message:`DM sessions require exactly two participants`})}),zs=es([`user`,`assistant`,`tool`,`system`]),Bs=Ko(`type`,[U({role:zs,type:ns(`text`),content:z(),timestamp:K,metadata:Ms.optional()}).passthrough(),U({role:zs,type:ns(`tool_call`),tool:G,params:js,timestamp:K,metadata:Ns}).passthrough(),U({role:zs,type:ns(`tool_result`),tool_id:G,result:js,ok:V(),timestamp:K,metadata:Ms.optional()}).passthrough(),U({role:zs,type:ns(`image`),url:G,alt:z().nullable(),timestamp:K}).passthrough()]),Vs=U({run_id:W,session_id:W,agent_id:W,status:Fs,lifecycle_revision:q,terminal_reason:z().optional(),response:z().optional(),error:z().optional(),started_at:K.nullable(),ended_at:K.nullable(),usage:Is.nullable(),ts:K,job_id:W.nullable(),parent_run_id:W.optional(),tool_call_count:q.optional(),queue_position:ks.optional(),resolved_config:Ms.optional()}).passthrough(),Hs=U({run_id:W,session_id:W,status:Fs,session_type:G,trigger:G,context_id:z(),ts:K,duration_ms:q.nullable().optional(),tool_call_count:q.nullable().optional(),usage:Is.nullable().optional()}).passthrough(),Us=Ko(`type`,[U({type:ns(`once`),run_at:K}).passthrough(),U({type:ns(`recurring`),cron:G}).passthrough()]),Ws=U({id:W,prompt:z(),schedule:Us,status:es([`pending`,`active`,`cancelled`]),next_run_at:K.nullable(),last_run_at:K.nullable()}).passthrough(),Gs=U({stream_epoch:W,retained_from:As,newest:As,replay_gap:V(),epoch_mismatch:V(),requires_reconciliation:V()}).passthrough(),Ks=U({session_id:W,run_id:W,agent_id:W,has_active_run:V(),ts:K}).passthrough(),qs=U({sessions:H(Rs)}).passthrough(),Js=U({agents:H(Ls)}).passthrough(),Ys=U({run_id:W,source_agent:G,kind:es([`reasoning`,`writing`,`tool_start`,`tool_end`]),tool:G.optional(),tool_invocation_id:G.optional(),parent_tool_invocation_id:G.optional()}).passthrough().superRefine((e,t)=>{e.kind===`tool_start`&&!e.tool&&t.addIssue({code:`custom`,path:[`tool`],message:`tool_start requires tool`}),(e.kind===`tool_start`||e.kind===`tool_end`)&&!e.tool_invocation_id&&t.addIssue({code:`custom`,path:[`tool_invocation_id`],message:`${e.kind} requires tool_invocation_id`})}),Xs=U({run_id:W,error:U({code:G,message:G}).passthrough()}).passthrough(),Zs=[[`run_created`,U({run_id:W,session_id:W,is_notification:V(),queued_behind:q,ts:K,source:G.optional()}).passthrough()],[`run_started`,U({run_id:W,session_id:W,ts:K,resolved_config:Ms.optional()}).passthrough()],[`run_queue_position`,U({run_id:W,session_id:W,agent_id:W,position:ks,ts:K}).passthrough()],[`status`,U({run_id:W,phase:G,ts:K,detail:z().optional()}).passthrough()],[`token_delta`,U({run_id:W,delta:z(),source_agent:G.optional()}).passthrough()],[`reasoning_delta`,U({run_id:W,text:z(),source_agent:G.optional()}).passthrough()],[`stream_reset`,U({run_id:W,source_agent:G.optional()}).passthrough()],[`tool_start`,U({run_id:W,tool_invocation_id:G,tool:G,params:js,source_agent:G.optional(),task_id:G.optional()}).passthrough()],[`tool_end`,U({run_id:W,tool_invocation_id:G,ok:V(),result:js,source_agent:G.optional(),task_id:G.optional()}).passthrough()],[`approval_required`,U({run_id:W,approval_id:W,capability:G,request:js,source_agent:G.optional()}).passthrough()],[`subagent_activity`,Ys],[`subagent_started`,U({session_id:W,tool_invocation_id:G,subagent_session_id:W,ts:K,subagent_name:G.optional()}).passthrough()],[`subagent_completed`,U({session_id:W,status:G,summary:z(),subagent_session_id:W,ts:K,tool_invocation_id:G.optional(),subagent_name:G.optional()}).passthrough()],[`job_completed`,U({session_id:W,job_name:G,status:G,summary:z(),run_id:W,job_id:W,job_session_id:G,truncated:V(),ts:K,job_session_uuid:W.optional()}).passthrough()],[`dm_message`,U({session_id:W,from_agent:G,from_agent_id:G,message:z(),ts:K}).passthrough()],[`dm_conversation_ended`,U({session_id:W,ended_by:G,peer:G,reason:G,context_id:z(),ts:K,suppress_banner:V().optional()}).passthrough()],[`dm_activity_started`,U({session_id:W,peer:G,ts:K}).passthrough()],[`dm_activity_status`,U({session_id:W,peer:G,phase:G,ts:K,detail:z().optional()}).passthrough()],[`dm_activity_ended`,U({session_id:W,peer:G,ts:K}).passthrough()],[`approval_resolved`,U({run_id:W,approval_id:W,decision:G,ts:K}).passthrough()],[`context_debug`,U({run_id:W,messages:js,tool_names:H(z()),total_tokens:q,system_tokens:q,history_message_count:q,agent_id:G,agent_name:z().nullable(),ts:K}).passthrough()],[`run_warning`,U({run_id:W,warning:U({code:G,message:G}).passthrough(),source_agent:G.optional()}).passthrough()],[`run_finished`,U({run_id:W,ok:V(),prompt_tokens:q,completion_tokens:q,reasoning_tokens:q.optional(),cache_creation_input_tokens:q.optional(),cache_read_input_tokens:q.optional(),ts:K}).passthrough()],[`run_error`,Xs],[`run_cancelled`,U({run_id:W,ts:K}).passthrough()],[`session_activity_started`,Ks],[`session_activity_ended`,Ks],[`stream_state`,Gs]],Qs=new Map(Zs);Object.freeze(Zs.map(([e])=>e));var $s=U({version:G,provider:G,model:G,posture:G,base_url:G,stream_chunk_timeout_secs:jo().nonnegative(),llm_providers:H(G),agents:H(U({id:W,name:G,is_default:V(),model:z().nullable(),needs_bootstrap:V()}).passthrough()),context:U({strategy:G,max_input_tokens:q,compact_trigger_pct:jo(),compact_retain_pct:jo(),summary_model:z().nullable(),summary_provider:z().nullable()}).passthrough(),session:U({max_messages:q,max_context_tokens:q,idle_timeout_secs:q,auto_archive:V(),archive_ttl_secs:q}).passthrough(),tools:U({sandbox_root:z(),shell_policy:G,timeout_secs:q,max_output_bytes:q,enabled:H(G)}).passthrough(),logging:U({file_enabled:V(),file_level:G,rotation:G,log_dir:z().nullable()}).passthrough(),llm:U({anthropic:U({thinking_budget_tokens:q,prompt_cache_enabled:V()}).passthrough(),openai:U({reasoning_effort:z().nullable()}).passthrough(),gemini:U({thinking_budget:q.nullable(),cache_enabled:V(),cache_ttl_seconds:q}).passthrough()}).passthrough()}).passthrough(),ec=[{method:`GET`,matches:e=>e.pathname===`/agents`,boundary:`GET /agents`,schema:Js},{method:`POST`,matches:e=>e.pathname===`/agents`,boundary:`POST /agents`,schema:Ls},{method:`GET`,matches:e=>/^\/agents\/[^/]+\/timeline$/.test(e.pathname),boundary:`GET /agents/{id}/timeline`,schema:U({agent_id:W,agent_name:G,events:H(U({timestamp:K,event_type:G,session_id:W,session_type:G,context_id:z(),run_id:W.optional(),summary:z(),metadata:Ms.optional()}).passthrough()),pagination:U({limit:q,has_more:V(),next_before:z().nullable()}).passthrough()}).passthrough()},{method:`GET`,matches:e=>/^\/agents\/[^/]+\/workspace$/.test(e.pathname),boundary:`GET /agents/{id}/workspace`,schema:U({agent_id:W,files:Qo(z(),z())}).passthrough()},{method:`PUT`,matches:e=>/^\/agents\/[^/]+\/workspace\/[^/]+$/.test(e.pathname),boundary:`PUT /agents/{id}/workspace/{file}`,schema:U({ok:Ps,file:G}).passthrough()},{method:`POST`,matches:e=>/^\/agents\/[^/]+\/workspace\/open$/.test(e.pathname),boundary:`POST /agents/{id}/workspace/open`,schema:U({ok:Ps,path:G}).passthrough()},{method:`POST`,matches:e=>/^\/agents\/[^/]+\/default$/.test(e.pathname),boundary:`POST /agents/{id}/default`,schema:U({ok:Ps,default_agent:G}).passthrough()},{method:`GET`,matches:e=>/^\/agents\/[^/]+$/.test(e.pathname),boundary:`GET /agents/{id}`,schema:Ls},{method:`PUT`,matches:e=>/^\/agents\/[^/]+$/.test(e.pathname),boundary:`PUT /agents/{id}`,schema:Ls},{method:`DELETE`,matches:e=>/^\/agents\/[^/]+$/.test(e.pathname),boundary:`DELETE /agents/{id}`,schema:U({ok:Ps,deleted:W}).passthrough()},{method:`GET`,matches:e=>e.pathname===`/sessions`,boundary:`GET /sessions`,schema:qs},{method:`POST`,matches:e=>e.pathname===`/sessions`,boundary:`POST /sessions`,schema:U({session_id:W,created:V()}).passthrough()},{method:`GET`,matches:e=>/^\/sessions\/[^/]+\/messages$/.test(e.pathname),boundary:`GET /sessions/{id}/messages`,schema:U({messages:H(Bs),last_event_id:As}).passthrough()},{method:`GET`,matches:e=>/^\/sessions\/[^/]+\/tool-calls$/.test(e.pathname),boundary:`GET /sessions/{id}/tool-calls`,schema:U({session_id:W,tool_calls:H(U({run_id:W,seq:q,role:es([`assistant`,`tool`]),timestamp:K,tool_name:G.optional(),tool_id:G.optional(),params:z().optional(),result:z().optional(),from_agent:G.optional()}).passthrough())}).passthrough()},{method:`POST`,matches:e=>/^\/sessions\/[^/]+\/cancel-dm$/.test(e.pathname),boundary:`POST /sessions/{id}/cancel-dm`,schema:U({ok:Ps,session_id:W,context_id:z(),participants:Xo([G,G]),runs_cancelled:q,reason:ns(`user_cancelled`)}).passthrough()},{method:`POST`,matches:e=>/^\/sessions\/[^/]+\/subagent\/cancel$/.test(e.pathname),boundary:`POST /sessions/{id}/subagent/cancel`,schema:U({session_id:W,status:ns(`cancelling`)}).passthrough()},{method:`DELETE`,matches:e=>/^\/sessions\/[^/]+$/.test(e.pathname),boundary:`DELETE /sessions/{id}`,schema:U({ok:Ps,deleted:W}).passthrough()},{method:`GET`,matches:e=>/^\/session\/[^/]+$/.test(e.pathname),boundary:`GET /session/{id}`,schema:Rs},{method:`POST`,matches:e=>e.pathname===`/runs`,boundary:`POST /runs`,schema:U({run_id:W,session_id:W,status:ns(`queued`),ts:K}).passthrough()},{method:`GET`,matches:e=>e.pathname===`/runs`&&e.searchParams.has(`agent_id`),boundary:`GET /runs?agent_id`,schema:U({runs:H(Hs)}).passthrough()},{method:`GET`,matches:e=>e.pathname===`/runs`,boundary:`GET /runs`,schema:U({runs:H(Vs)}).passthrough()},{method:`GET`,matches:e=>/^\/runs\/[^/]+\/reasoning$/.test(e.pathname),boundary:`GET /runs/{id}/reasoning`,schema:U({run_id:W,text:z(),last_event_id:As,terminal:V(),seal_event_id:As}).passthrough()},{method:`GET`,matches:e=>/^\/runs\/[^/]+\/text$/.test(e.pathname),boundary:`GET /runs/{id}/text`,schema:U({run_id:W,text:z(),last_event_id:As}).passthrough()},{method:`POST`,matches:e=>/^\/runs\/[^/]+\/cancel$/.test(e.pathname),boundary:`POST /runs/{id}/cancel`,schema:U({run_id:W,status:ns(`cancelled`)}).passthrough()},{method:`GET`,matches:e=>/^\/runs\/[^/]+$/.test(e.pathname),boundary:`GET /runs/{id}`,schema:Vs},{method:`GET`,matches:e=>e.pathname===`/approvals`,boundary:`GET /approvals`,schema:U({approvals:H(U({approval_id:W,run_id:W,tool:G,params:js,requested_at:K}).passthrough())}).passthrough()},{method:`POST`,matches:e=>/^\/approvals\/[^/]+$/.test(e.pathname),boundary:`POST /approvals/{id}`,schema:U({ok:Ps}).passthrough()},{method:`GET`,matches:e=>e.pathname===`/settings`,boundary:`GET /settings`,schema:$s},{method:`PATCH`,matches:e=>e.pathname===`/settings`,boundary:`PATCH /settings`,schema:U({status:ns(`ok`),restart_required:ns(!0).optional(),restart_reason:G.optional()}).passthrough()},{method:`GET`,matches:e=>e.pathname===`/auth/keys`,boundary:`GET /auth/keys`,schema:U({keys:H(U({provider:G,configured:V(),key:z().nullable(),source:es([`secrets`,`none`])}).passthrough())}).passthrough()},{method:`PUT`,matches:e=>e.pathname===`/auth/keys`,boundary:`PUT /auth/keys`,schema:U({ok:Ps,provider:G,key:G}).passthrough()},{method:`DELETE`,matches:e=>/^\/auth\/keys\/[^/]+$/.test(e.pathname),boundary:`DELETE /auth/keys/{provider}`,schema:U({ok:Ps,removed:V(),provider:G}).passthrough()},{method:`GET`,matches:e=>e.pathname===`/jobs`,boundary:`GET /jobs`,schema:U({jobs:H(Ws)}).passthrough()},{method:`POST`,matches:e=>e.pathname===`/jobs`,boundary:`POST /jobs`,schema:Ws},{method:`GET`,matches:e=>e.pathname===`/audit`,boundary:`GET /audit`,schema:U({events:H(U({session_id:W,run_id:W.nullable(),tool:G,decision:es([`allow`,`deny`,`error`]),params:js,result:js.nullable(),error:z().nullable(),timestamp:K}).passthrough())}).passthrough()}],tc=class extends Error{boundary;issues;constructor(e,t){let n=t.slice(0,3).map(e=>`${e.path.join(`.`)||`<root>`}: ${e.message}`).join(`; `);super(`Invalid ${e} payload: ${n}`),this.boundary=e,this.issues=t,this.name=`ContractViolation`}};function nc(e,t,n){let r=t.safeParse(n);if(!r.success)throw new tc(e,r.error.issues);return r.data}function rc(e,t){let n=Qs.get(e);if(!n)throw new tc(`SSE ${e}`,[{code:`custom`,path:[],message:`event type is not contracted`}]);return nc(`SSE ${e}`,n,t)}function ic(e,t,n){let r=new URL(e,`http://alms.local`),i=t.toUpperCase(),a=ec.find(e=>e.method===i&&e.matches(r));if(!a)throw new tc(`${i} ${r.pathname}`,[{code:`custom`,path:[],message:`route is not contracted`}]);return nc(a.boundary,a.schema,n)}function ac(e,t){try{return t()}catch(t){let n=t instanceof tc?t:new tc(e,[{code:`custom`,path:[],message:t instanceof Error?t.message:String(t),input:t}]);throw console.error(`[contract-boundary]`,n),Pe(n.message),n}}function oc(){let e={version:1,parseApiResponse:(e,t,n)=>ac(`${t.toUpperCase()} ${e}`,()=>ic(e,t,n)),parseApiJsonResponse:(e,t,n)=>ac(`${t.toUpperCase()} ${e} JSON`,()=>ic(e,t,JSON.parse(n))),parseSsePayload:(e,t)=>ac(`SSE ${e}`,()=>rc(e,t)),parseSseJsonPayload:(e,t)=>ac(`SSE ${e} JSON`,()=>rc(e,JSON.parse(t)))};return globalThis.__almsContracts=e,e}oc();var sc=()=>v(`/settings`),cc=e=>ce(`/settings`,e),lc=(e,t)=>{let n=new URLSearchParams;e&&n.set(`agent_id`,e),t&&t.includeDms&&n.set(`include_dms`,`true`);let r=n.toString();return v(`/sessions${r?`?`+r:``}`)},uc=(e,t)=>m(`/sessions`,{agent_id:e,context_id:t}),dc=e=>v(`/sessions/${e}/messages`),fc=e=>v(`/session/${e}`),pc=e=>g(`/sessions/${e}`),mc=e=>v(`/sessions/${e}/tool-calls`),hc=e=>m(`/sessions/${e}/cancel-dm`),gc=e=>m(`/sessions/${e}/subagent/cancel`),_c=d({});async function vc(){try{_c.value=await sc()}catch(e){console.error(`[settings] refresh failed:`,e)}}var yc=d(null),bc=d(null),xc=e({agentSwitchLoading:()=>Cc,bootRetryAvailable:()=>wc,runBoot:()=>Dc,sessionSwitchLoading:()=>Sc,setRunBoot:()=>Ec}),Sc=d(!1),Cc=d(!1),wc=d(!1),Tc=null;function Ec(e){Tc=e}function Dc(){Tc&&Tc()}var Oc=4e3,kc=d(null),Ac=null;function jc(){Ac&&=(clearTimeout(Ac),null)}function Mc(e){return!!(e&&e.status===`running`&&e.sessionId)}function Nc(e){return!!e&&kc.value===e}function Pc(e){return e?(jc(),kc.value=e,Ac=setTimeout(()=>{Ac=null,kc.value=null},Oc),!0):!1}function Fc(){jc(),kc.value=null}function Ic(e){e&&kc.value===e&&Fc()}async function Lc(e){if(!Nc(e))return!1;Fc();try{return await gc(e),!0}catch(t){return console.error(`[confirmSubagentCancel] cancel failed for session`,e,t),!1}}var Rc=`modulepreload`,zc=function(e){return`/ui/`+e},Bc={},Vc=function(e,t,n){let r=Promise.resolve();if(t&&t.length>0){let e=document.getElementsByTagName(`link`),i=document.querySelector(`meta[property=csp-nonce]`),a=i?.nonce||i?.getAttribute(`nonce`);function o(e){return Promise.all(e.map(e=>Promise.resolve(e).then(e=>({status:`fulfilled`,value:e}),e=>({status:`rejected`,reason:e}))))}function s(e){return import.meta.resolve?import.meta.resolve(e):new URL(e,import.meta.url).href}r=o(t.map(t=>{if(t=zc(t,n),t=s(t),t in Bc)return;Bc[t]=!0;let r=t.endsWith(`.css`);for(let n=e.length-1;n>=0;n--){let i=e[n];if(i.href===t&&(!r||i.rel===`stylesheet`))return}let i=document.createElement(`link`);if(i.rel=r?`stylesheet`:Rc,r||(i.as=`script`),i.crossOrigin=``,i.href=t,a&&i.setAttribute(`nonce`,a),document.head.appendChild(i),r)return new Promise((e,n)=>{i.addEventListener(`load`,e),i.addEventListener(`error`,()=>n(Error(`Unable to preload CSS for ${t}`)))})}))}function i(e){let t=new Event(`vite:preloadError`,{cancelable:!0});if(t.payload=e,window.dispatchEvent(t),!t.defaultPrevented)throw e}return r.then(t=>{for(let e of t||[])e.status===`rejected`&&i(e.reason);return e().catch(i)})},J=d({}),Y=new Map,Hc=8,Uc=3e4;function Wc(e,t,n,r,i){for(Y.delete(e),Y.set(e,{kind:t,tool:n||null,toolInvocationId:r||null,parentToolInvocationId:i||null,updatedAt:Date.now()});Y.size>Hc;){let e=Y.keys().next().value;Y.delete(e)}}function Gc(e){let t=Y.get(e);return!t||(Y.delete(e),Date.now()-t.updatedAt>Uc)?null:{kind:t.kind,tool:t.tool,toolInvocationId:t.toolInvocationId||null,parentToolInvocationId:t.parentToolInvocationId||null}}function Kc(e,t){if(t){for(let[e,n]of Y)if(n.parentToolInvocationId===t)return Gc(e)}if(!Y.get(e)?.parentToolInvocationId){let t=Gc(e);if(t)return t}if(!e.startsWith(`subagent-`))return null;for(let e of[...Y.keys()]){if(!e.startsWith(`subagent-`)||Y.get(e)?.parentToolInvocationId)continue;let t=Gc(e);if(t)return t}return null}var qc=d(null),Jc={},Yc=15e3,Xc=new Set([`done`,`fail`,`cancelled`]);function Zc(e){Jc[e]&&clearTimeout(Jc[e]),Jc[e]=setTimeout(()=>{delete Jc[e];let{[e]:t,...n}=J.value;t&&Ic(t.sessionId),J.value=n},Yc)}function Qc(){for(let[e,t]of Object.entries(J.value))Xc.has(t.status)&&!Jc[e]&&Zc(e)}function $c(e){if(!e)return{activity:null,toolsUsed:0,countedToolIds:new Set};let t=e.kind===`tool_start`;return{activity:{kind:e.kind,tool:e.tool},toolsUsed:+!!t,countedToolIds:t&&e.toolInvocationId?new Set([e.toolInvocationId]):new Set}}function el(e,t,n){let r=e===`subagent`&&n?`subagent-`+n.slice(0,8):e;Jc[r]&&(clearTimeout(Jc[r]),delete Jc[r]);let i=$c(Kc(r,n||null));J.value={...J.value,[r]:{status:`running`,task:t||``,toolInvocationId:n||null,displayName:e,startedAt:Date.now(),sessionId:null,activity:i.activity,toolsUsed:i.toolsUsed,countedToolIds:i.countedToolIds}}}var tl=Symbol(`drop-stale-subagent-signal`);function nl(e,t){if(t){let n=ol(t);if(n)return J.value[n]?.status===`running`?rl(n,e):tl;let r=J.value[e];return r?r.toolInvocationId?tl:e:null}if(J.value[e])return e;if(e.startsWith(`subagent-`)){for(let[t,n]of Object.entries(J.value))if(t.startsWith(`subagent-`)&&n.status===`running`)return rl(t,e)}return null}function rl(e,t){if(e===t||!t.startsWith(`subagent-`))return e;let{[e]:n,...r}=J.value;return J.value={...r,[t]:n},Jc[e]&&(clearTimeout(Jc[e]),delete Jc[e]),t}function il(e,t,n,r,i){if(!t)return;let a=nl(e,i);if(a===tl)return;if(!a){Wc(e,t,n,r,i);return}let o=J.value[a];if(!o)return;Y.delete(e);let s=o.countedToolIds instanceof Set?o.countedToolIds:new Set,c=o.toolsUsed||0,l=s;t===`tool_start`&&(r?s.has(r)||(l=new Set(s),l.add(r),c+=1):c+=1),J.value={...J.value,[a]:{...o,activity:{kind:t,tool:n||null},toolsUsed:c,countedToolIds:l}}}function al(e,t,n,r){Y.delete(e),Ic(r);let i=cl(e,n,r);if(!i)return;Y.delete(i);let a=J.value[i];a&&(Ic(a.sessionId),J.value={...J.value,[i]:{...a,status:t}},Zc(i))}function ol(e){if(!e)return null;for(let[t,n]of Object.entries(J.value))if(n.toolInvocationId===e)return t;return null}function sl(e){if(!e)return null;for(let[t,n]of Object.entries(J.value))if(n.sessionId===e)return t;return null}function cl(e,t,n){let r=ol(t);if(r)return r;let i=sl(n);if(i)return i;if(J.value[e])return e;if(e===`subagent`){for(let[e,t]of Object.entries(J.value))if(e.startsWith(`subagent-`)&&t.status===`running`)return e}return null}function ll(e,t,n){let r=cl(e,n,t);if(!r)return;let i=J.value[r];i&&(J.value={...J.value,[r]:{...i,sessionId:t}})}async function ul(){let[e,t,n,r,i,a,o,s]=await Promise.all([Vc(()=>Promise.resolve().then(()=>Ru),void 0),Vc(()=>Promise.resolve().then(()=>Bl),void 0),Vc(()=>import(`./chat-actions-UbdvPXnD.js`).then(e=>e.n),__vite__mapDeps([0,1,2])),Vc(()=>import(`./runs-D_HMln2i.js`).then(e=>e.a),__vite__mapDeps([3,1,2])),Vc(()=>import(`./select-generation-DvILpFQd.js`).then(e=>e.r),__vite__mapDeps([4,1])),Vc(()=>Promise.resolve().then(()=>xc),void 0),Vc(()=>Promise.resolve().then(()=>$u),void 0),Vc(()=>import(`./agents-Dxvrmzg8.js`).then(e=>e.i),__vite__mapDeps([5,1,2]))]);return{loadSession:e.loadSession,closeSessionStream:t.closeSessionStream,replaceMessages:n.replaceMessages,activeRunId:r.activeRunId,selectedRunId:r.selectedRunId,bumpSelectGeneration:i.bumpSelectGeneration,selectGeneration:i,sessionSwitchLoading:a.sessionSwitchLoading,saveActiveSession:o.saveActiveSession,activeAgentId:s.activeAgentId}}async function dl(e,t){let n=await ul(),r=n.bumpSelectGeneration();n.closeSessionStream(),x.value=e,n.activeRunId.value=null,n.selectedRunId.value=null,n.replaceMessages([]),ml(),n.activeAgentId.value&&n.saveActiveSession(n.activeAgentId.value,e),n.sessionSwitchLoading.value=!0;try{await n.loadSession(e,{isStale:()=>r!==n.selectGeneration.selectGeneration,logPrefix:t})}finally{r===n.selectGeneration.selectGeneration&&(n.sessionSwitchLoading.value=!1)}}function fl(e){if(!e)return;let t=x.value;t&&(qc.value=t),dl(e,`navigateToSubagent`).catch(e=>{console.error(`[navigateToSubagentSession] failed:`,e)})}function pl(){let e=qc.value;e&&(qc.value=null,dl(e,`navigateToParent`).catch(e=>{console.error(`[navigateToParentSession] failed:`,e)}))}function ml(){for(let e of Object.keys(Jc))clearTimeout(Jc[e]),delete Jc[e];Y.clear(),Fc(),J.value={}}function hl(e){if(Qc(),!Array.isArray(e)||e.length===0)return;let t=new Map,n=[],r={},i=new Map,a=-1/0,o=!1;for(let r of e){if(!o&&r&&typeof r.ts==`string`){let e=Date.parse(r.ts);Number.isFinite(e)&&(e<a?(console.warn(`[rehydrateSubagentsFromHistory] messages are not in chronological order; FIFO pairing of subagent invocations to completion markers may be wrong. See PR #1049 / Tim review suggestion 2.`),o=!0):a=e)}if(r.type===`subagent_started`){r.toolInvocationId&&r.subagentSessionId&&i.set(r.toolInvocationId,r.subagentSessionId);continue}if(r.type===`subagent_completed`){if(!r.sessionId)continue;let e=t.get(r.sessionId);if(e&&e.length>0){let t=e.shift();t.paired=!0}continue}if(r.type!==`tool`||r.tool!==`invoke_agent`)continue;let e=typeof r.result==`object`&&r.result||null,s=!!(e&&e.task_id),c=e?.session_id||null;if(s){let e={msg:r,paired:!1};if(n.push(e),c){let n=t.get(c);n||(n=[],t.set(c,n)),n.push(e)}continue}r.status===`running`&&n.push({msg:r,paired:!1})}for(let e of n){if(e.paired)continue;let t=e.msg,n=t.params||{},a=typeof t.result==`object`&&t.result||null,o=n.name||n.subagent_name||`subagent`,s=n.task||``,c=t.id||null,l=a?.session_id||c&&i.get(c)||null,u=o===`subagent`&&c?`subagent-`+String(c).slice(0,8):o;if(J.value[u]){!J.value[u].sessionId&&l&&ll(o,l,c);continue}let d=t.ts&&Date.parse(t.ts)||Date.now(),f=$c(Kc(u,c||null));r[u]={status:`running`,task:s,toolInvocationId:c,displayName:o,startedAt:d,sessionId:l,activity:f.activity,toolsUsed:f.toolsUsed,countedToolIds:f.countedToolIds}}Object.keys(r).length!==0&&(J.value={...J.value,...r})}var gl=d({phase:null,detail:null}),_l=d(null);function vl(e,t){if(!e)return null;switch(e){case`building_context`:return`Building context…`;case`summarizing`:return`Summarizing history…`;case`calling_llm`:return`Thinking…`;case`executing_tools`:return t?`Running ${t}\u2026`:`Running tools…`;case`tool_active`:return t?`Running ${t}\u2026`:`Running tool…`;case`dm`:return t?`Chatting with ${t}\u2026`:`In conversation…`;default:return null}}a(()=>{let{phase:e,detail:t}=gl.value;return vl(e,t)});function yl(e,t){gl.value={phase:e,detail:t||null}}function bl(){gl.value={phase:null,detail:null},_l.value=null}function xl(e){_l.value=e,gl.value={phase:`dm`,detail:e}}function Sl(e){_l.value?gl.value={phase:`dm`,detail:_l.value}:e&&(gl.value={phase:e,detail:null})}function Cl(e){return{approvalId:e.approval_id,tool:e.tool||e.capability,params:e.params||e.request,runId:e.run_id||null}}var wl={ignored:`no further replies`,depth_exceeded:`message limit reached`,user_cancelled:`cancelled by user`,errored:`run failed`},Tl=d(!1),El=new Set,Dl=()=>{},Ol=()=>{};function kl(e){if(!e||El.has(e))return;let t=El.size===0;El.add(e),t&&(Tl.value=!0)}function Al(e){e&&El.has(e)&&(El.delete(e),El.size===0&&(Tl.value=!1))}function jl(e){Dl=typeof e==`function`?e:()=>{}}function Ml(e){Ol=typeof e==`function`?e:()=>{}}function Nl(){try{Dl()}catch(e){console.error(`[stream-health] session reconnect failed:`,e)}try{Ol()}catch(e){console.error(`[stream-health] agent-events reconnect failed:`,e)}}function Pl(){Nl()}function Fl(){if(typeof window>`u`)return()=>{};let e=()=>Pl();return window.addEventListener(`online`,e),()=>window.removeEventListener(`online`,e)}var Il=new Map;function Ll(e,t){!e||!(t instanceof Set)||Il.set(e,t)}function Rl(e){return e&&Il.get(e)||null}function zl(e){if(e==null){Il.clear();return}Il.delete(e)}var Bl=e({closeSessionStream:()=>fu,dmThinkingBuffers:()=>Vl,isSessionStreamOpen:()=>pu,openSessionStream:()=>uu,registerSessionContractReconciler:()=>iu}),Vl=d(new Map),Hl=new Map,Ul=new Set;function Wl(e){if(e&&e.startsWith(`peer:`)){let t=e.slice(5),n=S.value;return n.length>=2?n[0]===t?n[1]:n[0]:null}return D.value?.name||null}function Gl(e){return ie.value?.session_type===`dm`?!0:e?Ul.has(e)?!0:A.value.some(t=>t.type===`dm_reasoning`&&t.runId===e):!1}function Kl(e,t){switch(e){case`AUTH`:return`Authentication failed -- check your API key in Settings.`;case`RATE_LIMIT`:return`Rate limited by the LLM provider -- wait a moment and try again.`;case`TIMEOUT`:return`Request timed out -- the LLM provider did not respond in time.`;default:return t}}var ql=null,Jl=null,Yl=0,Xl=10,Zl=null,Ql=``,$l=null,eu=-1,tu=!1,nu=null,ru=null;function iu(e){ru=e}function au(){if($l=null,!Ql)return;let e=Ql;Ql=``,we(t=>{let n=[...t.filter(e=>e.type!==`thinking`)],r=n[n.length-1];r&&r.type===`agent`&&!r.sealed?n[n.length-1]={...r,text:r.text+e}:n.push({id:k(),type:`agent`,role:`assistant`,text:e,sealed:!1,ts:new Date().toISOString()});let i=t.filter(e=>e.type===`tool`).length,a=n.filter(e=>e.type===`tool`).length;return a<i&&console.warn(`[flushDeltaBuffer] tool message count decreased:`,i,`->`,a),n})}function ou(){if($l===null){let e=eu;$l=requestAnimationFrame(()=>{if(e!==Oe){$l=null,Ql=``;return}c(au)})}}function su(e){if(!e)return;let t=Hl.get(e);if(!t)return;Hl.delete(e);let n=Vl.value,r=new Map(n);r.set(e,(r.get(e)||``)+t),Vl.value=r}function cu(e){e&&Hl.delete(e)}function lu(){let e=A.value,t=e.some(e=>e.type===`thinking`),n=t?e.filter(e=>e.type!==`thinking`):e,r=n[n.length-1];r&&r.type===`agent`&&!r.sealed?we(()=>{let t=[...n];t[t.length-1]={...r,sealed:!0};let i=e.filter(e=>e.type===`tool`).length,a=t.filter(e=>e.type===`tool`).length;return a<i&&console.warn(`[sealLastAgent] tool message count decreased:`,i,`->`,a),t}):t&&we(()=>{let t=e.filter(e=>e.type===`tool`).length,r=n.filter(e=>e.type===`tool`).length;return r<t&&console.warn(`[sealLastAgent] tool message count decreased:`,t,`->`,r),n})}function uu(e,t){let n=e&&(!t||!(t.sealedReasoningRunIds instanceof Set))?Rl(e):null,r=e&&e===Jl&&(!t||!(t.sealedReasoningRunIds instanceof Set))?new Map(Hl):null,i=e&&e===Jl&&(!t||!(t.sealedReasoningRunIds instanceof Set))?new Set(Ul):null;if(fu(),!e)return;Zl!==null&&(clearTimeout(Zl),Zl=null);let a=localStorage.getItem(`alms_auth_token`),o=new URLSearchParams;a&&o.set(`token`,a),t&&t.lastEventId!=null&&o.set(`last_event_id`,String(t.lastEventId));let s=o.toString(),l=`/sessions/${e}/events${s?`?`+s:``}`,u=new EventSource(l);ql=u,Jl=e,Yl=0,nu=t&&t.lastEventId!=null?t.lastEventId:null,t&&t.sealedReasoningRunIds instanceof Set?Ll(e,t.sealedReasoningRunIds):n instanceof Set&&Ll(e,n);let d=Rl(e);if(r instanceof Map)for(let[e,t]of r)Hl.set(e,t);if(i instanceof Set)for(let e of i)Ul.add(e);u.addEventListener(`open`,()=>{u===ql&&Al(`session`)}),eu=Oe;let f=new Set,p=!1,m=async(t,n)=>{if(!(p||eu!==Oe)){if(p=!0,u.close(),console.error(`[session-events] rejected live event; reconciling:`,n),!ru){kl(`session`);return}try{await ru(e,t)}catch(e){console.error(`[session-events] contract reconciliation failed:`,e),kl(`session`)}}},h=(e,t)=>u.addEventListener(e,n=>{if(eu!==Oe)return;let r=n.lastEventId,i=r&&/^\d+$/.test(r)?Number(r):null,a=r&&!r.startsWith(`ephemeral-`);if(!(a&&f.has(r)))try{let o=globalThis.__almsContracts;if(!o)throw Error(`Frontend contract bridge is not installed`);if(t({data:o.parseSseJsonPayload(e,n.data),lastEventId:n.lastEventId}),i!=null&&(nu=r),a&&f.add(r),f.size>2500){let e=0;for(let t of f){if(e++>=500)break;f.delete(t)}console.debug(`[sse-dedup] evicted`,500,`stale IDs, size:`,f.size)}}catch(e){let t=i??(/^\d+$/.test(String(nu))?Number(nu):null);m(t,e)}});h(`run_created`,e=>{let t=e.data,n=t.queued_behind;tu=!1,xe();let r=ie.value?.session_type===`dm`,i=!!(t.source&&t.source.startsWith(`peer:`));if(i&&(xl(t.source.slice(5)),Ul.add(t.run_id)),r||i)if(n>0)c(()=>{O.value=t.run_id,j({id:k(),type:`thinking`,source:t.source,queuedBehind:n,runId:t.run_id})});else{let e=Wl(t.source);c(()=>{O.value=t.run_id,j({id:k(),type:`dm_reasoning`,runId:t.run_id,agentName:e,thinkingText:``,tools:[],status:`running`,isLive:!0})})}else t.is_notification?c(()=>{O.value=t.run_id,j({id:k(),type:`thinking`,source:t.source,queuedBehind:n,runId:t.run_id})}):c(n>0?()=>{O.value=t.run_id,Ee(e=>e.type===`thinking`&&e.pending,e=>({...e,queuedBehind:n,pending:!1,runId:t.run_id}))}:()=>{O.value=t.run_id,Ee(e=>e.type===`thinking`&&e.pending,e=>({...e,pending:!1}))})}),h(`run_started`,e=>{let t=e.data;if(Gl(t.run_id)){let e=Wl(A.value.find(e=>e.type===`thinking`&&e.queuedBehind>0)?.source);we(n=>{let r=n.filter(e=>!(e.type===`thinking`&&e.queuedBehind>0));return r.some(e=>e.type===`dm_reasoning`&&e.runId===t.run_id)||r.push({id:k(),type:`dm_reasoning`,runId:t.run_id,agentName:e,thinkingText:``,tools:[],status:`running`,isLive:!0}),r})}else Ee(e=>e.type===`thinking`&&e.queuedBehind>0,e=>({...e,queuedBehind:0}));_l.value?yl(`dm`,_l.value):yl(`calling_llm`,null),xe()}),h(`run_queue_position`,e=>{let t=e.data,n=t.position;Ee(e=>e.type===`thinking`&&e.queuedBehind>0&&e.runId===t.run_id,e=>({...e,queuedBehind:n}))}),h(`status`,e=>{let t=e.data;if(console.debug(`[status]`,t.phase,t.detail||``),_l.value){yl(`dm`,_l.value);return}yl(t.phase,t.detail||null)}),h(`token_delta`,e=>{let t=e.data;if(!t.source_agent){if(Gl(t.run_id)){let e=t.run_id;Hl.set(e,(Hl.get(e)||``)+t.delta);return}tu=!0,Ql+=t.delta,ou()}}),h(`reasoning_delta`,e=>{let t=e.data;if(t.source_agent)return;let n=t.text||``;if(n){if(Gl(t.run_id)){let e=t.run_id,r=Vl.value,i=new Map(r);i.set(e,(i.get(e)||``)+n),Vl.value=i;return}t.run_id&&d&&d.has(t.run_id)||we(e=>{let t=[...e.filter(e=>e.type!==`thinking`)],r=t[t.length-1];return r&&r.type===`agent`&&!r.sealed?t[t.length-1]={...r,reasoning:(r.reasoning||``)+n}:t.push({id:k(),type:`agent`,role:`assistant`,text:``,reasoning:n,sealed:!1,ts:new Date().toISOString()}),t})}}),h(`stream_reset`,e=>{let t=e.data;if(t.source_agent)return;let n=t.run_id;c(()=>{if(Hl.delete(n),Vl.value.has(n)){let e=new Map(Vl.value);e.delete(n),Vl.value=e}Ql=``,we(e=>{let t=[...e],n=t[t.length-1];return n&&n.type===`agent`&&!n.sealed?(t.pop(),t):e})})}),h(`tool_start`,e=>{c(()=>{au();let t=e.data,n=t.tool_invocation_id,r=t.run_id,i=A.value.filter(e=>e.type===`tool`).length;console.debug(`[tool_start]`,t.tool,`id=`+n,`tool count before insertion:`,i);let a=Date.now();if(Gl(r)&&!t.source_agent){su(r);let e={id:n,type:`tool`,tool:t.tool,params:t.params,status:`running`,startedAt:a,runId:r};we(t=>{let n=t.findIndex(e=>e.type===`dm_reasoning`&&e.runId===r);if(n>=0){let r=t[n],i=[...t];return i[n]={...r,tools:[...r.tools,e]},i}return[...t,{id:k(),type:`dm_reasoning`,runId:r,agentName:null,thinkingText:``,tools:[e],status:`running`,isLive:!0}]})}else if(t.tool===`invoke_agent`){lu();let e=t.params?.name||t.params?.subagent_name||`subagent`,i=t.params?.task||``;j({id:n,type:`tool`,tool:`invoke_agent`,params:t.params,status:`running`,startedAt:a,runId:r}),el(e,i,n),t.subagent_session_id&&ll(ol(n)||e,t.subagent_session_id)}else t.source_agent||(lu(),j({id:n,type:`tool`,tool:t.tool,params:t.params,status:`running`,startedAt:a,runId:r}),_l.value||yl(`tool_active`,t.tool))})}),h(`tool_end`,e=>{c(()=>{let t=e.data,n=t.tool_invocation_id,r=t.ok?`done`:`fail`;if(t.source_agent)return;let i=Date.now(),a=e=>{let n=e.startedAt?i-e.startedAt:null;return{...e,status:r,result:t.result,durationMs:n}};if(Gl(t.run_id)&&n&&!t.source_agent){let e=!1;if(we(t=>{let r=[...t];for(let t=0;t<r.length;t++){let i=r[t];if(i.type!==`dm_reasoning`)continue;let o=i.tools.findIndex(e=>e.id===n);if(o>=0){let n=[...i.tools];n[o]=a(n[o]),r[t]={...i,tools:n},e=!0;break}}return r}),e){if(!t.source_agent){let{phase:e}=gl.value;(e===`tool_active`||e===`executing_tools`)&&Sl(`calling_llm`)}return}}let o=n&&Ee(e=>e.type===`tool`&&e.id===n,a);if(o||=Ee(e=>e.type===`tool`&&e.status===`running`,a),o){let e=A.value,i=n?e.find(e=>e.type===`tool`&&e.id===n):e.findLast(e=>e.type===`tool`&&e.status===r);if(i&&i.tool===`invoke_agent`){let e=typeof t.result==`object`?t.result:null;if(!(e&&e.task_id)){let t=i.params?.name||i.params?.subagent_name||ol(n);t&&(e&&e.session_id&&ll(t,e.session_id),al(t,r))}}}else t.source_agent||console.warn(`[tool_end] no matching tool message found for`,n,`- tool messages in chat:`,A.value.filter(e=>e.type===`tool`).length);if(!t.source_agent){let{phase:e}=gl.value;(e===`tool_active`||e===`executing_tools`)&&Sl(`calling_llm`)}})}),h(`approval_required`,e=>{c(()=>{au(),lu();let t=e.data;if(!A.value.some(e=>e.type===`approval`&&e.approvalId===t.approval_id)){let e=Cl(t);j({id:k(),type:`approval`,approvalId:e.approvalId,tool:e.tool,params:e.params,runId:e.runId,resolved:!1})}})}),h(`subagent_activity`,e=>{let t=e.data;il(t.source_agent,t.kind,t.tool||null,t.tool_invocation_id||null,t.parent_tool_invocation_id||null)}),h(`subagent_started`,e=>{c(()=>{let t=e.data,n=t.subagent_session_id,r=t.subagent_name||ol(t.tool_invocation_id);if(!r){console.warn(`[subagent_started] cannot resolve target entry`,`— subagent_name:`,t.subagent_name,`tool_invocation_id:`,t.tool_invocation_id);return}ll(r,n)})}),h(`subagent_completed`,e=>{c(()=>{let t=e.data,n=t.subagent_name||`subagent`,r=t.status||`done`,i=t.subagent_session_id||null,a=t.summary||``,o=t.tool_invocation_id||null,s=ol(o)||sl(i),c=s&&J.value[s]||J.value[n]||Object.values(J.value).find(e=>e.displayName===n||n===`subagent`&&e.status===`running`),l=c?c.task:``,u=c&&c.toolsUsed||0,d=c&&c.startedAt?Date.now()-c.startedAt:null;i&&ll(n,i,o),al(n,r,o,i),j({id:k(),type:`subagent_completed`,name:n,task:l,status:r,toolCount:u,durationMs:d,sessionId:i,summary:a})})}),h(`job_completed`,e=>{let t=e.data;j({id:k(),type:`job_completed`,jobName:t.job_name||`job`,status:t.status||`success`,summary:t.summary||``,ts:t.ts||null,runId:t.run_id||null,truncated:t.truncated,jobSessionUuid:t.job_session_uuid||null,jobSessionId:t.job_session_id||null})}),h(`dm_message`,e=>{c(()=>{au(),lu();let t=e.data;j({id:k(),type:`agent`,role:`assistant`,text:t.message,fromAgent:t.from_agent,fromAgentId:t.from_agent_id,sealed:!0,ts:t.ts||new Date().toISOString()})})}),h(`dm_conversation_ended`,e=>{let t=e.data,n=t.peer||`unknown`,r=wl[t.reason]||t.reason||`conversation ended`,i=t.suppress_banner===!0,a=t.context_id||null,o=!1;for(let e=A.value.length-1;e>=0;e--){let t=A.value[e];if(t.type===`agent`||t.type===`user`||t.type===`thinking`||t.type===`dm_message`||t.type===`dm_reasoning`)break;if(t.type===`dm_ended`){o=a&&t.contextId===a||t.peer===n&&t.reason===r;break}}!i&&!o&&j({id:k(),type:`dm_ended`,peer:n,reason:r,contextId:a}),bl()}),h(`dm_activity_started`,e=>{let t=e.data;t.peer&&xl(t.peer)}),h(`dm_activity_status`,e=>{let t=e.data,n=_l.value||t.peer;n&&yl(`dm`,n)}),h(`dm_activity_ended`,e=>{let t=e.data,n=_l.value||t.peer;n&&yl(`dm`,n)}),h(`approval_resolved`,e=>{let t=e.data;De(e=>!(e.type===`approval`&&e.approvalId===t.approval_id))}),h(`context_debug`,e=>{c(()=>{let t=e.data;j({id:k(),type:`context_debug`,messages:t.messages,toolNames:t.tool_names,totalTokens:t.total_tokens,systemTokens:t.system_tokens,historyMessageCount:t.history_message_count,agentId:t.agent_id,agentName:t.agent_name})})}),h(`run_warning`,e=>{let t=e.data;t.source_agent||c(()=>{au(),lu();let e=t.warning?.code||`UNKNOWN`,n=t.warning?.message||`Warning`;j({id:k(),type:`warning`,code:e,text:n})})});let g=t=>n=>{if(xe(),c(()=>{au(),lu();let r=n.data,i=r.run_id||null,a=Gl(i||O.value),o=``;if(a&&i){if(o=Vl.value.get(i)||``,o){let e=new Map(Vl.value);e.delete(i),Vl.value=e}cu(i)}we(e=>{let n=e.filter(e=>e.type===`tool`).length,s=e=>e.type===`approval`&&!e.resolved&&(!e.runId||!i||e.runId===i),c=e.filter(e=>!s(e)).map(e=>{if(e.type===`dm_reasoning`&&e.runId===i&&e.isLive){let n=o||e.thinkingText||``,r=t===`error`?`failed`:t===`cancelled`?`cancelled`:`done`,i=e.tools.map(e=>e.status===`running`&&r!==`done`?{...e,status:`cancelled`}:e);return{...e,status:r,isLive:!1,thinkingText:n,tools:i}}return e});if(t===`error`){let e=r.error?.code||`INTERNAL`,t=Kl(e,typeof r.error==`string`?r.error:r.error?.message||`Run failed`);c=[...c,{id:k(),type:`error`,code:e,text:t}]}t===`cancelled`&&(c=[...c,{id:k(),type:`system`,text:`(run cancelled)`}]),t===`finished`&&!tu&&!a&&(c=[...c,{id:k(),type:`system`,text:`(run completed)`}]);let l=r.prompt_tokens||r.completion_tokens?{prompt_tokens:r.prompt_tokens||0,completion_tokens:r.completion_tokens||0,reasoning_tokens:r.reasoning_tokens,cache_creation_input_tokens:r.cache_creation_input_tokens,cache_read_input_tokens:r.cache_read_input_tokens}:r.usage;l&&(c=[...c,{id:k(),type:`tokens`,usage:l}]);let u=c.filter(e=>e.type===`tool`).length;return u<n&&console.warn(`[handleRunEnd] tool message count decreased:`,n,`->`,u),c}),O.value=null,_l.value?yl(`dm`,_l.value):bl(),te(e)}),C.value.length>0){let t=C.value[0],n=C.value.slice(1);C.value=n;let r=x.value;me(e,n),Vc(()=>Promise.resolve().then(()=>Pp).then(e=>{e.startRun&&e.startRun(t.text,{sessionId:r})}),void 0).catch(e=>{console.error(`[session-stream] Failed to process queued message:`,e)})}};h(`run_finished`,g(`finished`)),h(`run_error`,g(`error`)),h(`run_cancelled`,g(`cancelled`)),u.onerror=()=>{if(u.readyState===EventSource.CLOSED){if(Yl++,Yl>=Xl){console.error(`[session-stream] Max retries reached`),kl(`session`);return}let t=Math.min(2e3*2**(Yl-1),3e4);Zl=setTimeout(()=>{Zl=null,x.value===e&&uu(e,{lastEventId:nu})},t)}}}function du(){Yl=0;let e=x.value;e?uu(e,{lastEventId:nu}):Al(`session`)}jl(du);function fu(){$l!==null&&(cancelAnimationFrame($l),$l=null),au(),Zl!==null&&(clearTimeout(Zl),Zl=null),tu=!1,Hl.clear(),Ul.clear(),bl(),Jl!=null&&(zl(Jl),Jl=null),ql&&=(ql.close(),null),Al(`session`)}function pu(){return ql!==null}var X=null,mu=null,hu=0,gu=10,_u=null,vu=null,yu=null;function bu(e,t){t.has_active_run?ae(t.session_id,{runId:e===`session_activity_ended`?null:t.run_id,finished:!1}):ue(t.session_id)}function xu(e){let t={};for(let n of e.sessions||[])n.has_active_run&&(t[n.id]={runId:null,finished:!1});re.value=t}function Su(e,t){let n=t&&t.streamEpoch!=null?String(t.streamEpoch):null;if(wu(),!e)return;yu!==null&&(clearTimeout(yu),yu=null);let r=localStorage.getItem(`alms_auth_token`),i=new URLSearchParams;r&&i.set(`token`,r),t&&t.lastEventId!=null&&i.set(`last_event_id`,String(t.lastEventId)),n&&i.set(`stream_epoch`,n);let a=i.toString(),o=`/events/session-activity${a?`?`+a:``}`,s=new EventSource(o);X=s,mu=e,hu=0,_u=t&&t.lastEventId!=null?t.lastEventId:null,vu=n;let c=!1,l=!1,u=[],d=!1,f=null,p=0,m=!1,h=(e,t,n)=>{if(l){u.push({type:e,data:t,eventId:n});return}bu(e,t)},g=async e=>{let t=Number.isSafeInteger(e)?e:null;if(l){d=!0,t!=null&&(f=f==null?t:Math.max(f,t));return}if(s!==X)return;l=!0,u=[];let n=null;try{n=await lc(null,{includeDms:!0})}catch(e){console.error(`[agent-events] activity reconciliation failed:`,e)}if(s!==X)return;n&&(xu(n),p=0);let r=u;u=[],l=!1;for(let e of r)t!=null&&e.eventId!=null&&e.eventId<=t||bu(e.type,e.data);let i=d,a=f;if(d=!1,f=null,i&&s===X){g(a);return}if(!n&&s===X){p++;let e=Math.min(1e3*2**(p-1),3e4);s._reconciliationRetryTimer=setTimeout(()=>{s._reconciliationRetryTimer=null,s===X&&g(null)},e)}};s.addEventListener(`open`,()=>{if(s!==X)return;let e=c||!!(t&&t.reconcileOnOpen);c=!0,Al(`agent-events`),e&&g(null)});let _=async(t,n)=>{if(!(m||s!==X)){m=!0,s.close(),console.error(`[agent-events] rejected live event; reconciling:`,n);try{let n=await lc(null,{includeDms:!0});if(s!==X)return;xu(n),X=null,Su(e,{lastEventId:t,streamEpoch:vu,reconcileOnOpen:!0})}catch(e){console.error(`[agent-events] contract reconciliation failed:`,e),kl(`agent-events`)}}},v=(e,t)=>s.addEventListener(e,n=>{if(s!==X)return;let r=n.lastEventId,i=r&&/^\d+$/.test(r)?Number(r):null;try{let a=globalThis.__almsContracts;if(!a)throw Error(`Frontend contract bridge is not installed`);t({data:a.parseSseJsonPayload(e,n.data),lastEventId:n.lastEventId}),i!=null&&(_u=r)}catch(t){i==null?(console.error(`[agent-events]`,e,`handler failed:`,t),s.close(),kl(`agent-events`)):_(i,t)}});v(`session_activity_started`,e=>{let t=e.data,n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;h(`session_activity_started`,t,n)}),v(`session_activity_ended`,e=>{let t=e.data,n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;h(`session_activity_ended`,t,n)}),v(`stream_state`,e=>{let t=e.data,n=!!(vu&&t.stream_epoch&&vu!==t.stream_epoch);if(t.stream_epoch&&(vu=t.stream_epoch),t.requires_reconciliation||n){let e=Number.isSafeInteger(t.newest)?t.newest:null;g(e)}}),s.onerror=()=>{if(s.readyState===EventSource.CLOSED){if(hu++,hu>=gu){console.error(`[agent-events] Max retries reached for agent`,e),kl(`agent-events`);return}let t=Math.min(2e3*2**(hu-1),3e4),n=e,r=_u,i=vu;yu=setTimeout(()=>{yu=null,mu===n&&Su(n,{lastEventId:r,streamEpoch:i,reconcileOnOpen:!0})},t)}}}function Cu(){hu=0;let e=mu;e?Su(e,{lastEventId:_u,streamEpoch:vu,reconcileOnOpen:!0}):Al(`agent-events`)}Ml(Cu);function wu(){X&&=(X._reconciliationRetryTimer!=null&&(clearTimeout(X._reconciliationRetryTimer),X._reconciliationRetryTimer=null),X.close(),null),mu=null,_u=null,vu=null,hu=0,yu!==null&&(clearTimeout(yu),yu=null),Al(`agent-events`)}var Tu=e=>m(`/runs`,e),Eu=e=>v(`/runs/${e}`),Du=(e,t=20)=>v(`/runs?session_id=${e}&limit=${t}`),Ou=e=>m(`/runs/${e}/cancel`),ku=e=>v(`/runs/${e}/reasoning`),Au=e=>v(`/runs/${e}/text`),ju=e=>v(`/approvals?session_id=${e}`),Mu=(e,t=50)=>v(`/runs?agent_id=${e}&limit=${t}`);function Nu(e,t){let n=e||``,r=t&&t.job_status,i=n.indexOf(`
`),a=i>=0?n.slice(0,i):n,o=i>=0?n.slice(i+1).trim():``,s=a.match(/^\[Scheduled job (\w+)\]\s*(.*)$/);if(!s)return{jobName:n,status:r||`success`,summary:``};let c=s[1];return{jobName:(s[2]||``).trim(),status:r||(c===`failed`?`error`:c===`completed`?`success`:c===`finished`?`cancelled`:`success`),summary:o}}function Pu(e){let t=new Map;for(let n of e){let e=n.tool_id;if(!e)continue;let r=t.get(e)||{call:null,result:null,runId:null};n.role===`assistant`||n.role===`Assistant`?(r.call=n,n.run_id&&(r.runId=n.run_id)):(n.role===`tool`||n.role===`Tool`)&&(r.result=n,n.run_id&&!r.runId&&(r.runId=n.run_id)),t.set(e,r)}return t}function Fu(e,t){let n=t&&t.hasActiveRun,r=t&&t.isDm,i=t&&t.sessionToolCalls||[],a=i.length>0?Pu(i):new Map,o=new Map,s=new Map;for(let t of e)t.type===`tool_result`&&t.tool_id&&o.set(t.tool_id,t),t.type===`tool_result`&&t.metadata&&t.metadata.tool_invocation_id&&s.set(t.metadata.tool_invocation_id,t);let c=[],l=[],u=(e,t)=>{c.push(e),l.push(t||null)};for(let t of e)if(t.type===`text`||!t.type){if(t.metadata&&t.metadata.message_type===`dm_ended`){let e=t.metadata.reason||``;u({id:k(),type:`dm_ended`,peer:t.metadata.ended_by||`unknown`,reason:wl[e]||e||`conversation ended`},t.timestamp);continue}if(t.metadata&&t.metadata.message_type===`reasoning`){let e=``;Array.isArray(t.metadata.reasoning_blocks)&&(e=t.metadata.reasoning_blocks.map(e=>e&&typeof e.text==`string`?e.text:``).join(``)),e||=t.content||``,u({id:k(),type:`dm_reasoning_text`,text:e,fromAgent:t.metadata.from_agent||null,runId:t.metadata.run_id||null},t.timestamp);continue}let e=t.role===`system`&&t.metadata&&t.metadata.synthetic,n=t.role===`user`&&t.metadata&&t.metadata.message_type===`dm`&&t.metadata.from_agent;if(e&&t.metadata.type===`job_notification`){let e=Nu(t.content||``,t.metadata);u({id:k(),type:`job_completed`,jobName:e.jobName,status:e.status,summary:e.summary,ts:t.timestamp||null,metadata:t.metadata||null,runId:t.metadata&&t.metadata.run_id||null,truncated:t.metadata?t.metadata.truncated:void 0,jobSessionUuid:t.metadata&&t.metadata.job_session_uuid||null,jobSessionId:t.metadata&&t.metadata.job_session_id||null},t.timestamp);continue}if(e&&t.metadata.type===`run_boundary`){let e=t.metadata.status||`completed`;u({id:k(),type:`run_boundary`,status:e,runId:t.metadata.run_id||null,error:t.metadata.error||null,text:t.content||``},t.timestamp);continue}if(e&&t.metadata.kind===`error`){u({id:k(),type:`error`,text:t.metadata.error?`${t.content}\n\n${t.metadata.error}`.trim():t.content||`Run error`,code:t.metadata.error_kind||t.metadata.type||null},t.timestamp);continue}if(e&&t.metadata.type===`run_warning`){if(t.metadata.source_agent)continue;u({id:k(),type:`warning`,code:t.metadata.code||`UNKNOWN`,text:t.content||`Warning`},t.timestamp);continue}if(e&&t.metadata.type===`subagent_completion`){let e=t.metadata;u({id:k(),type:`subagent_completed`,name:e.subagent_name||`subagent`,task:e.task_description||``,status:e.status||`done`,toolCount:e.tool_count||0,durationMs:e.duration_ms==null?null:e.duration_ms,sessionId:e.session_id||null,summary:e.summary||``,toolInvocationId:e.tool_invocation_id||null},t.timestamp);continue}if(e&&t.metadata.type===`subagent_started`){let e=t.metadata;u({id:k(),type:`subagent_started`,name:e.subagent_name||`subagent`,toolInvocationId:e.tool_invocation_id||null,subagentSessionId:e.subagent_session_id||null},t.timestamp);continue}if(e&&t.metadata.type===`dm_ended_notification`){u({id:k(),type:`notification`,role:`system`,text:t.content||``,metadata:t.metadata,sealed:!0},t.timestamp);continue}let r=e?`notification`:n?`agent`:t.role===`user`?`user`:`agent`,i;r===`agent`&&t.metadata&&Array.isArray(t.metadata.reasoning_blocks)&&(i=t.metadata.reasoning_blocks.map(e=>e&&typeof e.text==`string`?e.text:``).join(``),i||=void 0),u({id:k(),type:r,role:t.role,text:t.content||``,metadata:t.metadata||null,sealed:!0,reasoning:i,fromAgent:n?t.metadata.from_agent:void 0,ts:t.timestamp||null},t.timestamp)}else if(t.type===`tool_call`){let e=t.metadata&&t.metadata.tool_call_id||null,r=t.metadata&&t.metadata.tool_invocation_id||null,i=(e?o.get(e):null)||(r?s.get(r):null),c=t.metadata&&t.metadata.run_id||null,l=t.metadata&&t.metadata.message_type===`reasoning`,d=t.tool,f=t.params,p=i?i.result:null,m=i?i.ok:null,h=c;if(e&&a.has(e)){let t=a.get(e);if(t.call&&(d||=t.call.tool_name||d,!f&&t.call.params))try{f=JSON.parse(t.call.params)}catch{f=null}if(t.result&&p==null){try{p=JSON.parse(t.result.result)}catch{p=t.result.result}m??=!0}!h&&t.runId&&(h=t.runId)}let g=l&&t.metadata&&t.metadata.from_agent?t.metadata.from_agent:void 0;if(!g&&e&&a.has(e)){let t=a.get(e);g=t.call&&t.call.from_agent||t.result&&t.result.from_agent||void 0}u({id:r||e||k(),type:`tool`,tool:d,params:f,status:m==null?n?`running`:`done`:m?`done`:`fail`,result:p,runId:h||void 0,isReasoning:l||void 0,fromAgent:g,ts:t.timestamp||null},t.timestamp)}else if(t.type===`image`){let e=t.role===`user`&&t.metadata&&t.metadata.message_type===`dm`&&t.metadata.from_agent;u({id:k(),type:`image`,role:e?`assistant`:t.role,url:t.url||``,alt:t.alt||``,sealed:!0,fromAgent:e?t.metadata.from_agent:void 0,ts:t.timestamp||null},t.timestamp)}if(i.length>0){let t=new Set;for(let e of c)e.type===`tool`&&e.id&&t.add(e.id);for(let n of e)if(n.type===`tool_call`){let e=n.metadata&&n.metadata.tool_call_id;e&&t.add(e);let r=n.metadata&&n.metadata.tool_invocation_id;r&&t.add(r)}let i=[];for(let[e,o]of a){if(t.has(e)||!o.call)continue;let a=null;if(o.call.params)try{a=JSON.parse(o.call.params)}catch{}let s=null,c=null;if(o.result&&o.result.result){try{s=JSON.parse(o.result.result)}catch{s=o.result.result}c=!0}let l=o.call.from_agent||o.result&&o.result.from_agent||void 0;i.push({entry:{id:e||k(),type:`tool`,tool:o.call.tool_name||`unknown`,params:a,status:c==null?n?`running`:`done`:c?`done`:`fail`,result:s,runId:o.runId||void 0,isReasoning:r||void 0,fromAgent:l,ts:o.call.timestamp||null},ts:o.call.timestamp||null})}if(i.length>0){i.sort((e,t)=>!e.ts&&!t.ts?0:e.ts?t.ts?e.ts<t.ts?-1:+(e.ts>t.ts):-1:1);let e=0;for(let{entry:t,ts:n}of i){if(!n){c.push(t),l.push(null);continue}let r=c.length;for(let t=e;t<l.length;t++)if(l[t]&&l[t]>n){r=t;break}c.splice(r,0,t),l.splice(r,0,n),e=r+1}}}return c}function Iu(e){let t=new Map,n=new Set,r=null;for(let i=0;i<e.length;i++){let a=e[i];if(a.type===`dm_reasoning_text`&&a.runId){n.add(i);let e=t.get(a.runId)||{agentName:null,thinkingText:``,tools:[],firstIdx:i};e.agentName=e.agentName||a.fromAgent,e.thinkingText=(e.thinkingText||``)+(a.text||``),t.has(a.runId)||t.set(a.runId,e),r=a.runId;continue}if(a.type===`tool`&&a.runId){n.add(i);let e=t.get(a.runId)||{agentName:null,thinkingText:``,tools:[],firstIdx:i};e.tools.push(a),e.agentName=e.agentName||a.fromAgent,t.has(a.runId)||t.set(a.runId,e),r=a.runId;continue}if(a.type===`tool`&&!a.runId&&r&&t.has(r)){n.add(i);let e=t.get(r);e.tools.push(a),e.agentName=e.agentName||a.fromAgent;continue}a.type!==`tool`&&(r=null)}if(t.size===0)return e;let i=[],a=new Set;for(let r=0;r<e.length;r++){if(n.has(r)){let n=e[r].runId;if(n&&t.has(n)&&!a.has(n)){let e=t.get(n);(e.tools.length>0||e.thinkingText&&e.thinkingText.trim())&&i.push({id:k(),type:`dm_reasoning`,runId:n,agentName:e.agentName,thinkingText:e.thinkingText||``,tools:e.tools,status:`done`,isLive:!1}),a.add(n)}continue}i.push(e[r])}return i}function Lu(e,t){return typeof t==`number`&&Number.isFinite(t)&&e!=null&&Number.isFinite(Number(e))&&Number(e)>=t}var Ru=e({loadSession:()=>Uu}),zu=new Set([`subagent`,`job`,`episodic`,`notification`]),Bu=200,Vu=100;function Hu(e,t){return t||_.value.find(t=>t.id===e)||w.value.find(t=>t.id===e)||null}async function Uu(e,t){let n=t.isStale,r=t.logPrefix||`loadSession`,i=null;try{let t=await fc(e);if(n())return;i=t||null,t&&zu.has(t.session_type)&&!_.value.some(e=>e.id===t.id)&&(_.value=[..._.value,t]),t&&t.session_type===`dm`&&!w.value.some(e=>e.id===t.id)&&(w.value=[...w.value,t]),t&&Object.prototype.hasOwnProperty.call(t,`parent_session_id`)?qc.value=t.parent_session_id??null:qc.value=null}catch(e){if(n())return;if(console.warn(`[${r}] Failed to fetch session metadata:`,e),t.requireAuthoritativeSnapshot)throw e}try{let t=await Du(e,Bu);if(n())return;let r=t.runs||[];be.value=r;let i=r.find(e=>e.status===`running`)||r.find(e=>e.status===`queued`);i&&(O.value=i.run_id)}catch(e){if(n())return;if(t.requireAuthoritativeSnapshot)throw e;be.value=[]}let a=t.minimumLastEventId??null,o=!1,s=new Set;try{let[s,c]=await Promise.all([dc(e),mc(e).catch(e=>{if(console.warn(`[${r}] Failed to load session tool calls:`,e),t.requireAuthoritativeSnapshot)throw e;return{tool_calls:[]}})]);if(n())return;let l=s.messages||[],u=c.tool_calls||[];o=Hu(e,i)?.session_type===`dm`;let d=Fu(l,{hasActiveRun:!!O.value,sessionToolCalls:u,isDm:o}),f=l.filter(e=>e.type===`tool_call`).length,p=d.filter(e=>e.type===`tool`).length;(f>0||p>0||u.length>0)&&console.debug(`[${r}] history loaded:`,l.length,`API messages,`,f,`tool_calls ->`,p,`tool rows,`,u.length,`session tool call records`);let m=le(e);if(m){let t=!1;if(m.runId)t=!!be.value.find(e=>e.run_id===m.runId);else{let e=d.findLast(e=>e.type===`user`);t=e&&e.text===m.text}t?te(e):(d.push({id:k(),type:`user`,role:`user`,text:m.text,sealed:!0,ts:m.ts||new Date().toISOString()}),console.debug(`[${r}] re-injected pending user message for session`,e))}Te(o?Iu(d):d),hl(d),a=s.last_event_id??null}catch(e){if(n())return;if(Te([{id:k(),type:`error`,text:`Failed to load message history: ${e.error?.message||e.message||`unknown error`}`}]),t.requireAuthoritativeSnapshot)throw e}if(O.value){if(!A.value.some(e=>e.type===`thinking`)){let e=be.value.find(e=>e.run_id===O.value),i=e&&e.status===`queued`,a=+!!i;if(i)try{let e=await Eu(O.value);if(n())return;typeof e?.queue_position==`number`&&e.queue_position>0&&(a=e.queue_position)}catch(e){if(console.warn(`[${r}] Failed to load queue position:`,e),t.requireAuthoritativeSnapshot)throw e}j({id:k(),type:`thinking`,queuedBehind:a,runId:O.value})}try{let t=await ju(e);if(n())return;let r=t.approvals||[];r.length>0&&j(...r.map(e=>{let t=Cl(e);return{id:k(),type:`approval`,approvalId:t.approvalId,tool:t.tool,params:t.params,runId:t.runId,resolved:!1}}))}catch(e){if(console.warn(`[${r}] Failed to load pending approvals:`,e),t.requireAuthoritativeSnapshot)throw e}{let e=O.value,i=be.value.find(t=>t.run_id===e),c=i&&(i.status===`running`||i.status===`queued`);try{let t=await ku(e);if(n())return;t?.terminal===!0&&(Lu(a,t.seal_event_id)&&s.add(e),O.value=null,De(t=>!(t.type===`thinking`&&t.runId===e))),o||(c&&t?.text&&j({id:k(),type:`agent`,role:`assistant`,text:``,reasoning:t.text,sealed:!1,ts:new Date().toISOString()}),t?.last_event_id!=null&&(a==null||t.last_event_id>a)&&(a=t.last_event_id))}catch(e){if(console.warn(`[${r}] Failed to load in-flight reasoning:`,e),t.requireAuthoritativeSnapshot)throw e}if(!o)try{let t=await Au(e);if(n())return;c&&t?.text&&(Ee(e=>e.type===`agent`&&!e.sealed,e=>({...e,text:(e.text||``)+t.text}))||j({id:k(),type:`agent`,role:`assistant`,text:t.text,reasoning:``,sealed:!1,ts:new Date().toISOString()})),t?.last_event_id!=null&&(a==null||t.last_event_id>a)&&(a=t.last_event_id)}catch(e){if(console.warn(`[${r}] Failed to load in-flight text:`,e),t.requireAuthoritativeSnapshot)throw e}}}if(!n())if(uu(e,{lastEventId:a,sealedReasoningRunIds:s}),O.value){let t=be.value.find(e=>e.run_id===O.value);if(!(t&&t.status===`queued`)){let t=Hu(e,i);if(t&&t.session_type===`dm`&&Array.isArray(t.participants)){let e=D.value?.name,n=e?t.participants.find(t=>t!==e):t.participants[0];n?xl(n):yl(`calling_llm`,null)}else yl(`calling_llm`,null)}}else{let t=D.value?.id;t?Wu(t,e,n,r).catch(e=>console.warn(`[${r}] restoreGlobalAgentPhase uncaught:`,e)):bl()}}async function Wu(e,t,n,r){try{let t=await Mu(e,Vu);if(n())return;let i=t.runs||[],a=i.find(e=>e.session_type===`dm`&&e.status===`running`);if(a&&a.context_id){let e=D.value?.name,t=a.context_id.split(`:`);if(t.length>=3&&t[0]===`dm`&&e){let n=t[1]===e?t[2]:t[1];if(n){xl(n),console.debug(`[${r}] restored cross-session DM status: Chatting with ${n}`);return}}}i.find(e=>e.status===`running`)?yl(`calling_llm`,null):bl()}catch(e){console.warn(`[${r}] Failed to check agent global status:`,e),bl()}}iu(async(e,t)=>{await Uu(e,{isStale:()=>x.value!==e,logPrefix:`contract-reconcile`,minimumLastEventId:t,requireAuthoritativeSnapshot:!0})});function Gu(e,t){return!t||typeof t!=`string`?e??null:t===e?null:t}function Ku(e){return!e||typeof e!=`string`?null:e}function qu(e,t){return!t||typeof t!=`string`||!e||typeof e!=`string`?!1:e===t}function Ju(e){let t=new Map;if(!Array.isArray(e))return t;for(let n of e){if(!n||typeof n.agent_id!=`string`||!n.agent_id)continue;let e=t.get(n.agent_id);e?e.push(n):t.set(n.agent_id,[n])}return t}function Yu(e,t){if(!e||!t||typeof t!=`string`)return!1;if(e.session_type===`notification`)return e.agent_name===t;if(e.session_type===`dm`){let n=e.participants;return Array.isArray(n)&&n.includes(t)}return!1}function Xu(e,t){if(!Array.isArray(e))return[];let n=e.map((e,n)=>({s:e,idx:n,owned:+!Yu(e,t)}));return n.sort((e,t)=>e.owned-t.owned||e.idx-t.idx),n.map(e=>e.s)}function Zu(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type!==`notification`&&e.session_type!==`job`):[]}function Qu(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type===`job`):[]}var $u=e({boot:()=>od,fetchCrossAgentSurfaces:()=>sd,saveActiveSession:()=>rd,switchAgent:()=>ld}),ed=`alms_active_agent`,td=0;function nd(e){return`alms_active_session_${e}`}function rd(e,t){e&&t&&localStorage.setItem(nd(e),t)}function id(e,t,n){if(n){let e=t.find(e=>e.id===n);if(e)return e}let r=localStorage.getItem(nd(e));if(r){let e=t.find(e=>e.id===r);if(e)return e}return t[0]||null}async function ad(e,t){let n=localStorage.getItem(nd(e));if(!n||t.some(e=>e.id===n))return null;try{return await dc(n),n}catch(t){return t&&t.status===404&&localStorage.removeItem(nd(e)),null}}async function od(){try{let e=await sc();_c.value=e,E.value=e.agents||[];let t=localStorage.getItem(ed),n=E.value.find(e=>e.is_default),r=E.value[0],i=E.value.find(e=>e.id===t)||n||r;i&&(T.value=i.id,ye.value=Ku(i.id),localStorage.setItem(ed,i.id),await cd(i.id))}catch(e){throw console.error(`[boot] failed:`,e),e}}async function sd(){try{return(await lc(null,{includeDms:!0})).sessions||[]}catch(e){return console.error(`[fetchCrossAgentSurfaces] failed:`,e),[]}}async function cd(e,t){let n=++td;try{let[r,i]=await Promise.all([lc(e,{includeDms:!1}),sd()]);if(n!==td)return;let a=Zu(r.sessions||[]);_.value=a,w.value=i;let o={};for(let e of[...a,...i])e.has_active_run&&(o[e.id]={runId:null,finished:!1});re.value=o,Su(e);let s=t?null:await ad(e,a);if(n!==td)return;if(s)x.value=s,rd(e,s),await Uu(s,{isStale:()=>n!==td,logPrefix:`loadAgentSessions:hidden`});else if(a.length>0){let r=id(e,a,t);x.value=r.id,rd(e,r.id),await Uu(r.id,{isStale:()=>n!==td,logPrefix:`loadAgentSessions`})}else{let t=await uc(e,`web-chat-`+Date.now());if(n!==td)return;let[r,i]=await Promise.all([lc(e,{includeDms:!1}),sd()]);if(n!==td)return;_.value=Zu(r.sessions||[]),w.value=i,x.value=t.session_id,Te([]),be.value=[],uu(t.session_id)}}catch(e){if(n!==td)return;console.error(`[loadAgentSessions] failed:`,e)}}async function ld(e,t){if(!E.value.find(t=>t.id===e))return;fu(),wu(),ke(),T.value=e,ye.value=Ku(e),localStorage.setItem(ed,e),Cc.value=!0,x.value=null,O.value=null,Se.value=null,_.value=[],w.value=[],be.value=[],Te([]),C.value=[],yc.value=null,bc.value=null,re.value={},ml();let n=cd(e,t&&t.targetSessionId),r=td;try{await n}finally{r===td&&(Cc.value=!1)}}var ud=d(null),Z=d(`agents`);function dd(e){ud.value===e?ud.value=null:(ud.value=e,Z.value=e)}var fd=`alms_theme`;function pd(){return localStorage.getItem(fd)||`dark`}var md=d(pd());function hd(){let e=md.value===`dark`?`light`:`dark`;md.value=e,localStorage.setItem(fd,e),document.documentElement.setAttribute(`data-theme`,e)}document.documentElement.setAttribute(`data-theme`,pd());var gd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="8"/><path d="M10 6v4l3 3"/></svg>`,_d=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 5a2 2 0 012-2h3l2 2h5a2 2 0 012 2v7a2 2 0 01-2 2H5a2 2 0 01-2-2V5z"/></svg>`,vd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M5 5l10 10M15 5L5 15"/></svg>`,yd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 15V5M10 5L5 10M10 5l5 5"/></svg>`,bd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor"><rect x="5" y="5" width="10" height="10" rx="1.5"/></svg>`,xd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l10 6-10 6V4z"/></svg>`,Sd=()=>f`<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 00-2 2v.18a2 2 0 01-1 1.73l-.43.25a2 2 0 01-2 0l-.15-.08a2 2 0 00-2.73.73l-.22.38a2 2 0 00.73 2.73l.15.1a2 2 0 011 1.72v.51a2 2 0 01-1 1.74l-.15.09a2 2 0 00-.73 2.73l.22.38a2 2 0 002.73.73l.15-.08a2 2 0 012 0l.43.25a2 2 0 011 1.73V20a2 2 0 002 2h.44a2 2 0 002-2v-.18a2 2 0 011-1.73l.43-.25a2 2 0 012 0l.15.08a2 2 0 002.73-.73l.22-.39a2 2 0 00-.73-2.73l-.15-.08a2 2 0 01-1-1.74v-.5a2 2 0 011-1.74l.15-.09a2 2 0 00.73-2.73l-.22-.38a2 2 0 00-2.73-.73l-.15.08a2 2 0 01-2 0l-.43-.25a2 2 0 01-1-1.73V4a2 2 0 00-2-2z"/><circle cx="12" cy="12" r="3"/></svg>`,Cd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M3 5h14M3 10h14M3 15h14"/></svg>`,wd=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="4"/><path d="M10 2v2M10 16v2M3.5 10H2M18 10h-1.5M5.05 5.05L3.63 3.63M16.37 16.37l-1.42-1.42M5.05 14.95l-1.42 1.42M16.37 3.63l-1.42 1.42"/></svg>`,Td=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 12.5A7.5 7.5 0 017.5 3 7.5 7.5 0 1017 12.5z"/></svg>`,Ed=d(!1);function Dd(){Ed.value=!Ed.value}function Od(){Ed.value=!1}var kd=[`agents`,`jobs`,`audit`],Ad=a(()=>_c.value.posture||`guarded`);function jd({onOpenSettings:e,status:t}){let n=Ad.value,r=t.value===`connected`?`ok`:t.value===`running`?`running`:t.value===`error`||t.value===`offline`?`error`:``;return f`
        <header>
            <button class="sidebar-toggle-btn" title="Toggle sessions" aria-label="Toggle sessions"
                    onClick=${Dd}>
                ${Ed.value?f`<${vd} />`:f`<${Cd} />`}
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
            ${wc.value&&f`
                <button class="retry-btn" onClick=${Dc}>Retry</button>
            `}

            <div class="header-btns">
                ${kd.map(e=>f`
                    <button class="hbtn ${ud.value===e?`active`:``}"
                            onClick=${()=>dd(e)}>
                        ${e.charAt(0).toUpperCase()+e.slice(1)}
                    </button>
                `)}
            </div>

            <button class="header-icon-btn" title="Toggle theme" aria-label="Toggle theme"
                    onClick=${hd}>
                ${md.value===`dark`?f`<${wd} />`:f`<${Td} />`}
            </button>

            <button class="header-icon-btn settings-btn" title="Settings" aria-label="Settings"
                    onClick=${e}>
                <${Sd} />
            </button>
        </header>
    `}async function Md(e,t){if(!e||e===x.value)return;Od();let n=_.value.find(t=>t.id===e)||w.value.find(t=>t.id===e);if(n&&n.session_type!==`dm`&&n.session_type!==`notification`&&n.session_type!==`job`&&n.agent_id&&T.value&&n.agent_id!==T.value&&E.value.some(e=>e.id===n.agent_id)){await ld(n.agent_id,{targetSessionId:e});return}let r=ke();fu(),c(()=>{x.value=e,O.value=null,Se.value=null,Te([]),C.value=[],bc.value=null,ml(),qc.value=null,Sc.value=!0}),rd(T.value,e);try{await Uu(e,{isStale:()=>r!==Oe,logPrefix:t&&t.logPrefix||`navigateToSession`})}finally{r===Oe&&(Sc.value=!1)}}var Nd={chat:{icon:`▸`,cls:``,label:`Chat session`},dm:{icon:`↔`,cls:`dm`,label:`DM conversation`},notification:{icon:`⚡`,cls:`notification`,label:`Notification session`},job:{icon:`⏰`,cls:`job`,label:`Job session`},subagent:{icon:`⚙`,cls:`subagent`,label:`Subagent session`},telegram:{icon:`✉`,cls:`telegram`,label:`Telegram session`}};function Pd(e){return Nd[e.session_type]||Nd.chat}function Fd(e,t,n,r){if(e===t&&n)return!0;let i=r[e];return!!(i&&!i.finished)}function Id(e){return Md(e,{logPrefix:`selectSession`})}async function Ld(){if(T.value){fu(),ke();try{let e=`web-chat-`+Date.now(),t=await uc(T.value,e),[n,r]=await Promise.all([lc(T.value,{includeDms:!0}),sd()]);c(()=>{_.value=n.sessions||[],w.value=r,x.value=t.session_id,rd(T.value,t.session_id),O.value=null,Se.value=null,Te([]),C.value=[],be.value=[],bc.value=null,ml()}),uu(t.session_id)}catch(e){console.error(`[newSession] failed:`,e)}}}function Rd(e){let t=e.participants;return Array.isArray(t)&&t.length>=2?t.join(` <-> `):e.context_id||e.id.slice(0,8)}function zd(e){return e.agent_name?`notifications`:e.context_id||e.id.slice(0,8)}function Bd(e){let t=e.context_id||``;return t.startsWith(`job_`)&&t.length>4?`job `+t.slice(4,12):t||e.id.slice(0,8)}function Vd(e){return e.session_type===`dm`?Rd(e):e.session_type===`notification`?zd(e):e.session_type===`job`?Bd(e):e.context_id||e.id.slice(0,8)}function Hd(e){if(e.session_type===`notification`&&e.agent_name)return e.agent_name;if(e.session_type===`job`&&e.agent_id){let t=E.value.find(t=>t.id===e.agent_id);return t?t.name:null}return null}function Ud({session:e,activeAgentName:t}){let n=u(!1),r=u(null),i=x.value,a=e.id===i,o=Fd(e.id,i,O.value,re.value),s=Pd(e),l=s.cls?` session-item-`+s.cls:``,d=e=>{e.stopPropagation(),n.value=!0,r.value=setTimeout(()=>{n.value=!1},3e3)},p=async t=>{t.stopPropagation(),r.value&&=(clearTimeout(r.value),null),n.value=!1;try{await pc(e.id),he(e.id),e.id===x.value&&(fu(),c(()=>{x.value=null,O.value=null,Se.value=null,Te([]),be.value=[],bc.value=null,ml(),C.value=[]}));let[t,n]=await Promise.all([lc(T.value,{includeDms:!0}),sd()]);_.value=t.sessions||[],w.value=n}catch(e){console.error(`[deleteSession] failed:`,e)}},m=e=>{e.stopPropagation(),r.value&&=(clearTimeout(r.value),null),n.value=!1},h=Vd(e),g=e.session_type===`chat`?``:`
Type: `+e.session_type,v=Hd(e);return f`
        <div class="session-item${l}${Yu(e,t)?` session-item-active-agent`:``} ${a?`active`:``} ${o?`has-run`:``}"
             role="option"
             aria-selected=${a}
             tabindex="0"
             title=${`ID: `+e.id+`
Context: `+e.context_id+g}
             onClick=${()=>Id(e.id)}
             onKeyDown=${t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),Id(e.id))}}>
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
    `}function Wd({label:e,cls:t,id:n}){return f`
        <div class="session-section-divider ${t||``}" role="presentation" id=${n}>
            <span class="session-section-divider-label">${e}</span>
        </div>
    `}function Gd({expanded:e,count:t,headerId:n}){let r=e=>{e.stopPropagation(),b.value=!b.value};return f`
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
    `}function Kd(e){let t=Gu(ye.value,e);ye.value=t,t&&t!==T.value&&ld(t)}function qd({agent:e,expanded:t,sessionCount:n,isActive:r,headerId:i}){let a=t=>{t.stopPropagation(),Kd(e.id)},o=t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),Kd(e.id))},s=r?t?`Collapse sessions`:`Expand sessions`:`Switch to `+e.name;return f`
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
    `}function Jd(e){let t=new Set,n=[];for(let r of e){if(r.session_type!==`dm`||!Array.isArray(r.participants)||r.participants.length<2)continue;let e=r.context_id||r.id;t.has(e)||(t.add(e),n.push(r))}return n}function Yd(){let e=_.value,t=w.value,n=E.value,r=T.value,i=ye.value,a=D.value?D.value.name:null,o=Ju(e.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`&&e.session_type!==`subagent`&&e.session_type!==`episodic`)),s=Ju(t.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`)),c=Xu(Jd(t),a),l=Xu(t.filter(e=>e.session_type===`notification`),a),u=Qu(t),d=b.value;return f`
        <div class="sidebar-section" style="flex:1; min-height:0">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${(!n||n.length===0)&&c.length===0&&l.length===0&&u.length===0?f`<div class="empty-state">No sessions</div>`:null}
                ${n.map(e=>{let t=qu(i,e.id),n=e.id===r,c=o.get(e.id)||[],l=n?c.length:(s.get(e.id)||[]).length,u=`agent-group-header-`+e.id;return f`
                        <div class="agent-group" key=${e.id}>
                            <${qd}
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
                                            <${Ud} key=${e.id} session=${e} activeAgentName=${a} />
                                        `)}
                                </div>
                            </div>
                        </div>
                    `})}
                ${c.length>0&&f`
                    <${Wd} label="Direct messages"
                                       cls="session-divider-dm"
                                       id="session-section-dms" />
                    <div role="group" aria-labelledby="session-section-dms">
                        ${c.map(e=>f`
                            <${Ud} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${l.length>0&&f`
                    <${Wd} label="Notifications"
                                       cls="session-divider-notification"
                                       id="session-section-notifications" />
                    <div role="group" aria-labelledby="session-section-notifications">
                        ${l.map(e=>f`
                            <${Ud} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${u.length>0&&f`
                    <${Gd} expanded=${d}
                                          count=${u.length}
                                          headerId="session-section-jobs" />
                    <div class="agent-group-body"
                         role="group"
                         aria-labelledby="session-section-jobs"
                         data-expanded=${d}>
                        <div class="agent-group-sessions">
                            ${u.map(e=>f`
                                <${Ud} key=${e.id} session=${e} activeAgentName=${a} />
                            `)}
                        </div>
                    </div>
                `}
            </div>
            <button id="new-session-btn" onClick=${Ld}>+ New session</button>
        </div>
    `}function Xd(){let e=Ed.value?` sidebar-open`:``;return f`
        ${Ed.value&&f`<div class="sidebar-backdrop" onClick=${Od}></div>`}
        <div id="sidebar" class=${e}>
            <${Yd} />
        </div>
    `}function Zd(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function Qd(e){if(!e)return``;let t=new Date(e),n=new Date;return t.toDateString()===n.toDateString()?Zd(e):t.toLocaleDateString([],{month:`short`,day:`numeric`})}function $d(e){if(!e)return``;let t=new Date(e);if(isNaN(t.getTime()))return``;let n=new Date,r=t.toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`});return t.toDateString()===n.toDateString()?r:`${t.toLocaleDateString([],{month:`short`,day:`numeric`})} ${r}`}function ef(e){e&&(e.scrollTop=e.scrollHeight)}function tf(e){if(!e)return``;let t=``;if(typeof e.querySelector==`function`){let n=e.querySelector(`code`);n&&typeof n.textContent==`string`&&(t=n.textContent)}return!t&&typeof e.textContent==`string`&&(t=e.textContent),t?(t.endsWith(`\r
`)?t=t.slice(0,-2):t.endsWith(`
`)&&(t=t.slice(0,-1)),t):``}function nf(){return!!(typeof navigator<`u`&&navigator.clipboard&&typeof navigator.clipboard.writeText==`function`)}var rf=`cb-copy-decorated`,af=`code-block-wrapper`,of=`code-block-copy`,sf=`code-block-copy--copied`,cf=`alms-code-copy-live`;function lf(){if(typeof document>`u`)return null;let e=document.getElementById(cf);return e||(e=document.createElement(`div`),e.id=cf,e.setAttribute(`aria-live`,`polite`),e.setAttribute(`role`,`status`),e.style.position=`absolute`,e.style.width=`1px`,e.style.height=`1px`,e.style.padding=`0`,e.style.margin=`-1px`,e.style.overflow=`hidden`,e.style.clip=`rect(0, 0, 0, 0)`,e.style.whiteSpace=`nowrap`,e.style.border=`0`,document.body.appendChild(e),e)}function uf(){let e=lf();e&&(e.textContent=``,setTimeout(()=>{e.textContent=`Copied to clipboard`},50))}function df(){return[`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" `,`stroke="currentColor" stroke-width="1.5" stroke-linecap="round" `,`stroke-linejoin="round" aria-hidden="true">`,`<rect x="7" y="7" width="10" height="10" rx="1.5"/>`,`<path d="M5 13H4a1 1 0 01-1-1V4a1 1 0 011-1h8a1 1 0 011 1v1"/>`,`</svg>`].join(``)}function ff(){return[`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" `,`stroke="currentColor" stroke-width="2" stroke-linecap="round" `,`stroke-linejoin="round" aria-hidden="true">`,`<path d="M4 10l4 4 8-8"/>`,`</svg>`].join(``)}function pf(e){if(typeof document>`u`)return!1;let t=document.createElement(`textarea`);t.value=e,t.style.position=`fixed`,t.style.top=`-9999px`,t.style.left=`-9999px`,t.setAttribute(`readonly`,``),t.setAttribute(`aria-hidden`,`true`),document.body.appendChild(t);let n=!1;try{t.select(),t.setSelectionRange(0,e.length),n=document.execCommand&&document.execCommand(`copy`)}catch{n=!1}return document.body.removeChild(t),!!n}function mf(e){e&&(e._copyRevertTimer&&=(clearTimeout(e._copyRevertTimer),null),e.classList.add(sf),e.innerHTML=ff(),e.setAttribute(`aria-label`,`Copied`),e.title=`Copied`,uf(),e._copyRevertTimer=setTimeout(()=>{e.classList.remove(sf),e.innerHTML=df(),e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e._copyRevertTimer=null},1500))}function hf(e,t,n){e.preventDefault(),e.stopPropagation();let r=tf(t);if(r){if(nf()){navigator.clipboard.writeText(r).then(()=>mf(n),()=>{pf(r)&&mf(n)});return}pf(r)&&mf(n)}}function gf(e,t=`pre`){if(!e||typeof e.querySelectorAll!=`function`)return;let n=e.querySelectorAll(`.${af}`);for(let e=0;e<n.length;e++){let t=n[e];if(!t.parentNode)continue;let r=t.querySelector(`pre`);if(!r){t.parentNode.removeChild(t);continue}if(!r.classList.contains(rf)){let e=t.parentNode,n=Array.from(t.childNodes);for(let r=0;r<n.length;r++){let i=n[r];i.nodeType===1&&i.classList&&i.classList.contains(of)||e.insertBefore(i,t)}e.removeChild(t)}}let r=e.querySelectorAll(t);for(let e=0;e<r.length;e++){let t=r[e];if(t.classList.contains(rf))continue;let n=t.parentNode;if(!n)continue;if(n.classList&&n.classList.contains(af)){if(!n.querySelector(`.${of}`)){let e=document.createElement(`button`);e.type=`button`,e.className=of,e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e.innerHTML=df(),e.addEventListener(`click`,n=>hf(n,t,e)),n.appendChild(e)}t.classList.add(rf);continue}if(!((t.textContent||``).trim().length>0)){t.classList.add(rf);continue}let i=document.createElement(`div`);i.className=af,n.insertBefore(i,t),i.appendChild(t);let a=document.createElement(`button`);a.type=`button`,a.className=of,a.setAttribute(`aria-label`,`Copy code`),a.title=`Copy code`,a.innerHTML=df(),a.addEventListener(`click`,e=>hf(e,t,a)),i.appendChild(a),t.classList.add(rf)}}function _f({ts:e}){if(!e)return null;let t=$d(e);return t?f`<span class="msg-timestamp" title=${e}>${t}</span>`:null}function vf({text:e,live:t}){let n=u(!1);if(!e)return null;let r=()=>{n.value=!n.value},i=e.length>0?` (${e.length} chars)`:``,a=t?`Thinking…`:`Reasoning`,o=n.value?`▼`:`▶`;return f`
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
    `}function yf({html:e}){let t=n(null);return p(()=>{gf(t.current)},[e]),f`
        <div class="msg-body markdown-body" ref=${t}
             dangerouslySetInnerHTML=${{__html:e}} />
    `}function bf({type:e,role:t,text:n,sealed:r,fromAgent:i,reasoning:a,ts:o}){let c=e===`user`?`user`:`agent`,l=_e.value||D.value?.name,u=e===`user`?`>`:i?`${i} $`:l?`${l} $`:`$`,d=e===`agent`&&r===!1,p=e===`agent`&&a?f`<${vf} text=${a} live=${d} />`:null,m=typeof n==`string`&&n.trim().length>0,h=o&&!d;if(e===`agent`&&r){let e=m?s(n):``;return f`
            <div class="msg ${c}">
                <div class="msg-label-row">
                    <div class="msg-label">${u}</div>
                    ${h&&f`<${_f} ts=${o} />`}
                </div>
                ${p}
                ${m&&f`<${yf} html=${e} />`}
            </div>
        `}return f`
        <div class="msg ${c}">
            <div class="msg-label-row">
                <div class="msg-label">${u}</div>
                ${h&&f`<${_f} ts=${o} />`}
            </div>
            ${p}
            ${(m||d)&&f`
                <div class="msg-body ${d?`streaming-cursor`:``}">${n}</div>
            `}
        </div>
    `}function xf({usage:e}){if(!e)return null;let t=e.prompt_tokens||0,n=e.completion_tokens||0;if(t+n===0)return null;let r=e.reasoning_tokens;return f`<div class="msg-tokens">${t}p + ${n}c${typeof r==`number`&&r>0?` + ${r}r`:``} tokens</div>`}function Sf({text:e,code:t}){return f`
        <div class="msg msg-error ${t?`msg-error--${t.toLowerCase()}`:``}" data-code=${t||``}>
            <div class="msg-error-icon">\u274C</div>
            <div class="msg-error-body">
                <div class="msg-error-title">Error</div>
                <div class="msg-error-text">${e}</div>
            </div>
        </div>
    `}function Cf({id:e,text:t,code:n}){let r=u(!1),i=u(!1);return i.value?null:f`
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
                    <button class="msg-warning-dismiss" onClick=${t=>{t.stopPropagation(),i.value=!0,e&&De(t=>t.id!==e)}}
                            title="Dismiss" aria-label="Dismiss warning">
                        \u2715
                    </button>
                </div>
                ${!r.value&&f`
                    <div class="msg-warning-text">${t}</div>
                `}
            </div>
        </div>
    `}function wf({text:e}){return f`
        <div class="msg-system">
            ${e}
        </div>
    `}function Tf({status:e,error:t}){return!e||e===`completed`?f`<div class="run-boundary run-boundary--completed" />`:f`
        <div class="run-boundary ${e===`failed`?`run-boundary--failed`:e===`cancelled`?`run-boundary--cancelled`:``}">
            <span class="run-boundary-label">${e===`failed`?`run failed`:e===`cancelled`?`run cancelled`:`run ${e}`}</span>
        </div>
        ${e===`failed`&&t&&f`
            <div class="run-boundary-error">${t}</div>
        `}
    `}function Ef({peer:e,reason:t}){return f`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">DM conversation with ${e} ended</span>
            <span class="dm-ended-reason">${t}</span>
        </div>
    `}function Df(e,t){if(!t)return``;switch(e){case`shell`:case`shell_exec`:return t.command?t.command:t.argv?t.argv.join(` `):``;case`fs_read`:return t.path||``;case`fs_write`:return`${t.mode===`append`?`(append) `:``}${t.path||``}`;case`fs_list`:return t.path||`.`;case`workspace_write`:return`${t.file||``}: ${(t.content||``).slice(0,60)}`;case`http_get`:if(!t.url)return``;try{return new URL(t.url).hostname+` `+t.url}catch{return t.url}case`math`:return t.operation?t.operation+`(`+[t.a,t.b,t.n].filter(e=>e!==void 0).join(`, `)+`)`:``;case`echo`:return t.message||t.text||``;case`send_message`:return t.to?`to ${t.to}`:``;case`invoke_agent`:{let e=t.name||t.subagent_name||``,n=t.task||``;return e&&n?`${e}: ${n.length>60?n.slice(0,60)+`…`:n}`:e}case`read_session`:return(t.session_id?t.session_id.slice(0,8)+`…`:``)+(t.last_n?` (last ${t.last_n})`:``);case`read_subagent_session`:return(t.name||``)+(t.last_n?` (last ${t.last_n})`:``);case`list_agents`:case`list_my_sessions`:return``;case`read_messages`:return t.from?`from ${t.from}`:``;case`ignore_message`:return t.from?`from ${t.from}`:``;default:{let e=Object.entries(t);return e.map(([t,n])=>{let r=typeof n==`string`?n:JSON.stringify(n);return e.length>1?`${t}=${r}`:r}).join(` `)}}}function Of(e){return e<1024?e+` B`:e<1024*1024?(e/1024).toFixed(1)+` KB`:(e/(1024*1024)).toFixed(1)+` MB`}var kf=2e3,Af=800;function jf(e){if(!e)return``;let t=e.replace(/\\/g,`/`).split(`/`).filter(Boolean);return t.length<=2?t.join(`/`):`…/`+t.slice(-2).join(`/`)}function Q(e){return typeof e==`object`&&!!e&&typeof e.error==`string`}function Mf(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.task_id==`string`?e.task_id:null,n=typeof e.status==`string`?e.status:null,r=typeof e.command==`string`?e.command:null,i=typeof e.exit_code==`number`?e.exit_code:null,a=typeof e.stdout==`string`?e.stdout:``,o=typeof e.stderr==`string`?e.stderr:``,s=typeof e.error==`string`?e.error:null,c=typeof e.message==`string`?e.message:null;return t&&(n===`submitted`||n===`unknown`||n===`not_found_or_still_running`)?f`
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
    `}function Nf(e,t){if(typeof e!=`object`||!e||Q(e)||typeof e.content!=`string`)return null;let n=t&&t.path||``,r=typeof e.lines_returned==`number`?e.lines_returned:null,i=typeof e.total_lines==`number`?e.total_lines:null,a=e.has_more_before===!0,o=e.has_more_after===!0,s=typeof e.note==`string`?e.note:null,c=e.byte_budget_exceeded===!0,l=e.line_truncated===!0,u=[];r!=null&&i!=null?r===i?u.push(`${r} lines (full file)`):u.push(`${r} of ${i} lines`):r!=null&&u.push(`${r} lines`),a&&u.push(`more before`),o&&u.push(`more after`),c&&u.push(`byte-budget exceeded`),l&&u.push(`per-line truncated`);let d=u.join(` · `),p=e.content||``;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">
                ${n?jf(n):`File content`}
            </div>
            ${p?f`<pre class="tc-detail-content tc-code-block">${p}</pre>`:f`<pre class="tc-detail-content tc-detail-muted">${s||`(empty)`}</pre>`}
            ${d&&f`<div class="tc-detail-footer">${d}</div>`}
            ${p&&s&&f`<div class="tc-detail-footer">${s}</div>`}
        </div>
    `}function Pf(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.path==`string`&&e.path||typeof e.file==`string`&&e.file||null,n=typeof e.replacements==`number`?e.replacements:null,r=typeof e.mode==`string`?e.mode:null,i=e.ok===!0;return t?f`
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
    `:null}function Ff(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.matches))return null;let t=e.matches,n=e.truncated===!0,r=typeof e.truncated_lines==`number`&&e.truncated_lines>0?e.truncated_lines:0;if(t.length===0)return f`
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
    `}function If(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.files))return null;let t=e.files,n=typeof e.total==`number`?e.total:t.length,r=e.truncated===!0;if(t.length===0)return f`
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
    `}function Lf(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.entries))return null;let t=typeof e.path==`string`?e.path:``,n=e.entries;return n.length===0?f`
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
    `}function Rf(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.status==`number`?e.status:null,a=typeof e.content_type==`string`?e.content_type:null,o=e.body;if(i==null&&o===void 0)return null;let s=a&&a.toLowerCase().includes(`application/json`),c;if(typeof o==`string`)c=o;else if(o==null)c=``;else try{c=JSON.stringify(o,null,2)}catch{c=String(o)}let l=c.length>kf,u=r&&!r.value&&l?c.slice(0,kf)+`…`:c,d=e=>{e.stopPropagation(),r&&(r.value=!r.value)},p=i!=null&&i>=200&&i<400?`tc-kv-badge`:`tc-kv-badge tc-kv-badge-fail`,m=[];if(e.headers&&typeof e.headers==`object`&&!Array.isArray(e.headers)){let t=Object.keys(e.headers).sort();for(let n of t){let t=e.headers[n];if(Array.isArray(t))for(let e of t)m.push([n,typeof e==`string`?e:String(e)]);else typeof t==`string`&&m.push([n,t])}}let h=m.length;return f`
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
    `}function zf(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.task_id==`string`?e.task_id:null,a=typeof e.session_id==`string`?e.session_id:null,o=typeof e.response==`string`?e.response:``,s=t&&(t.name||t.subagent_name)||``;if(i)return f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Subagent (background)</div>
                <div class="tc-status-row">
                    ${s&&f`<span class="tc-kv-badge">${s}</span>`}
                    <span class="tc-kv-mono">task_id: ${i}</span>
                </div>
                ${a&&f`
                    <button class="tc-detail-link"
                        type="button"
                        onClick=${e=>{e.stopPropagation(),Md(a,{logPrefix:`invokeAgentLink`})}}>
                        View full session
                    </button>
                `}
            </div>
        `;if(!o&&!a)return null;let c=o.length>Af,l=r&&!r.value&&c?o.slice(0,Af)+`…`:o;return f`
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
                    onClick=${e=>{e.stopPropagation(),Md(a,{logPrefix:`invokeAgentLink`})}}>
                    View full session
                </button>
            </div>
        `}
    `}function Bf(e){if(typeof e!=`object`||!e||Q(e))return null;let t=e.delivered===!0,n=typeof e.dm_session_id==`string`?e.dm_session_id:null,r=typeof e.note==`string`?e.note:null;return!t&&!n?null:f`
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
    `}function Vf(e,t,n){let r=n&&n.tool;if(typeof e!=`object`||!e||Q(e))return null;let i=Array.isArray(e.messages)?e.messages:null,a=typeof e.summary==`string`&&e.summary.length>0?e.summary:null,o=typeof e.peer==`string`?e.peer:null,s=typeof e.subagent==`string`?e.subagent:null,c=typeof e.session_id==`string`?e.session_id:null,l=typeof e.note==`string`&&e.note.length>0?e.note:null,u=typeof e.message_count==`number`?e.message_count:typeof e.fallback_message_count==`number`?e.fallback_message_count:null,d=typeof e.showing==`number`?e.showing:typeof e.fallback_showing==`number`?e.fallback_showing:i?i.length:null;if(!i&&a){let e=[];return u!=null&&e.push(`${u} messages total`),c&&e.push(`session: ${c.slice(0,8)}…`),f`
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
    `}function Hf(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.agents))return null;let t=e.agents;return t.length===0?f`
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
    `}function Uf(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.sessions))return null;let t=e.sessions,n=typeof e.total==`number`?e.total:t.length,r=typeof e.showing==`number`?e.showing:t.length;if(t.length===0)return f`
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
    `}function Wf(e){if(Q(e))return null;let t;if(typeof e==`string`)t=e;else if(e&&typeof e==`object`)try{t=JSON.stringify(e,null,2)}catch{return null}else if(typeof e==`number`||typeof e==`boolean`)t=String(e);else return null;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Echoed</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `}function Gf(e){return Q(e)||typeof e!=`number`?null:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${e}</span>
            </div>
        </div>
    `}function Kf(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.iso==`string`?e.iso:null,n=typeof e.human==`string`?e.human:null,r=typeof e.timezone==`string`?e.timezone:null,i=typeof e.local_iso==`string`?e.local_iso:null,a=typeof e.local_human==`string`?e.local_human:null,o=typeof e.local_timezone==`string`?e.local_timezone:null,s=typeof e.utc_offset==`string`?e.utc_offset:null;return!t&&!i?null:f`
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
    `}function qf(e){if(typeof e!=`object`||!e||Q(e)||e.ignored!==!0)return null;let t=typeof e.reason==`string`?e.reason:``;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Ignored</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">ignored</span>
                ${t&&f`<span class="tc-kv-meta">${t}</span>`}
            </div>
        </div>
    `}function Jf(e,t,n,r){if(t==null)return null;switch(e){case`shell`:case`shell_exec`:return Mf(t);case`fs_read`:return Nf(t,n);case`fs_write`:case`workspace_write`:case`fs_edit`:return Pf(t);case`fs_grep`:return Ff(t);case`fs_glob`:return If(t);case`fs_list`:return Lf(t);case`http_get`:return Rf(t,n,r);case`invoke_agent`:return zf(t,n,r);case`send_message`:return Bf(t);case`read_messages`:case`read_session`:case`read_subagent_session`:return Vf(t,n,{...r,tool:e});case`list_agents`:return Hf(t);case`list_my_sessions`:return Uf(t);case`ignore_message`:return qf(t);case`echo`:return Wf(t);case`math`:return Gf(t);case`datetime`:return Kf(t);default:return null}}var Yf=500,Xf=200;function Zf(e,t){return typeof e==`string`?e.length<=t?e:e.slice(0,t)+`…`:``}var Qf={fs_edit:[`old_string`,`new_string`],invoke_agent:[`task`],send_message:[`message`],ignore_message:[`reason`],echo:[`message`,`text`]};function $f(e,t){if(!t||typeof t!=`object`)return!1;let n=Qf[e];if(!n)return!1;for(let e of n){let n=t[e];if(typeof n==`string`&&n.length>Xf)return!0}return!1}function ep(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function tp(e){if(e==null)return``;if(typeof e==`string`)try{let t=JSON.parse(e);return JSON.stringify(t,null,2)}catch{return e}return JSON.stringify(e,null,2)}function np(e){if(e==null)return 0;let t=typeof e==`string`?e:JSON.stringify(e);return new Blob([t]).size}function rp(e){switch(e){case`shell`:case`shell_exec`:return`$`;case`fs_read`:return`R`;case`fs_write`:return`W`;case`fs_list`:return`L`;case`workspace_write`:return`W`;case`http_get`:return`H`;case`send_message`:return`DM`;case`invoke_agent`:return`IA`;case`read_session`:case`read_subagent_session`:return`RS`;case`list_agents`:return`LA`;case`list_my_sessions`:return`LS`;case`read_messages`:return`RM`;case`ignore_message`:return`IG`;case`math`:return`#`;case`echo`:return`E`;default:return`T`}}function ip(e,t){if(!t)return null;switch(e){case`shell`:case`shell_exec`:if(t.command)return f`
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
                        <pre class="tc-detail-content tc-code-block">${Zf(t.old_string,Xf)}</pre>
                    </div>
                `}
                ${t.new_string&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Replace with</div>
                        <pre class="tc-detail-content tc-code-block">${Zf(t.new_string,Xf)}</pre>
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
                        <pre class="tc-detail-content">${Zf(t.task,Xf)}</pre>
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
                        <pre class="tc-detail-content">${Zf(t.message,Xf)}</pre>
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
                    ${e?f`<pre class="tc-detail-content">${Zf(e,Xf)}</pre>`:f`<div class="tc-detail-footer">no reason given</div>`}
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
                    <pre class="tc-detail-content">${Zf(t.message||t.text||``,Xf)}</pre>
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
            `}}let n=tp(t);return n?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters</div>
            <pre class="tc-detail-content">${n}</pre>
        </div>
    `:null}function ap({params:e}){let t=tp(e);return t?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters (raw)</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `:null}function op({tool:e,params:t,panelRef:n}){let r=u(!1);p(()=>{gf(n?.current,`pre.tc-code-block`)});let i=ip(e,t);return i?$f(e,t)?f`
        ${r.value?f`<${ap} params=${t} />`:i}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),r.value=!r.value}}>
                ${r.value?`Hide raw params`:`View raw params`}
            </button>
        </div>
    `:i:null}function sp({result:e,isFail:t,showFull:n,label:r,blockedTarget:i}){let a=tp(e);if(!a)return null;let o=a.length>Yf,s=!n.value&&o?a.slice(0,Yf)+`…`:a,c=n.value?` tc-detail-expanded`:``;return f`
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
    `}function cp({tool:e,params:t,result:n,isFail:r,isCancelled:i,showFull:a,panelRef:o}){let s=u(!1);if(p(()=>{gf(o?.current,`pre.tc-code-block`)}),n==null&&!r)return null;let c=r&&typeof n==`object`&&n&&typeof n.target==`string`&&n.target.length>0?n.target:null;if(r)return f`<${sp} result=${n} isFail=${!0}
            showFull=${a} label="Error" blockedTarget=${c} />`;if(i)return f`<${sp} result=${n} isFail=${!1}
            showFull=${a} label="Result (cancelled)" />`;let l=Jf(e,n,t,{showFull:a});return l?f`
        ${s.value?f`<${sp} result=${n} isFail=${!1}
                showFull=${a} label="Result (raw)" />`:l}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),s.value=!s.value}}>
                ${s.value?`Hide raw`:`View raw`}
            </button>
        </div>
    `:f`<${sp} result=${n} isFail=${!1}
            showFull=${a} label="Result" />`}function lp({tool:e,params:t,status:r,result:i,id:a,sourceAgent:o,durationMs:s}){let c=u(!1),l=u(!1),d=e=>{e.stopPropagation(),c.value=!c.value},m=n(null);p(()=>{c.value&&gf(m.current,`pre.tc-code-block`)});let h=Df(e,t),g=h.length>80?h.slice(0,80)+`…`:h,_=r===`running`,v=r===`fail`,y=r===`done`,b=r===`cancelled`,x=e===`send_message`,ee=v?`tc-fail`:y?`tc-done`:b?`tc-cancelled`:`tc-running`,S=c.value?`▼`:`▶`,te=rp(e),ne=ep(s),re=i==null?0:np(i),ie=re>=100?Of(re):``;return f`
        <div class="tc-row ${ee} ${x?`tc-dm`:``}" role="button" tabindex="0"
             onClick=${d} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),d(e))}}>
            <div class="tc-header">
                <span class="tc-chevron">${S}</span>
                ${_?f`<span class="tc-spinner"></span>`:f`<span class="tc-icon">${te}</span>`}
                <span class="tc-name">${e}</span>
                ${g&&f`<span class="tc-summary">${g}</span>`}
                <span class="tc-spacer"></span>
                ${ie&&f`<span class="tc-result-size">${ie}</span>`}
                ${ne&&f`<span class="tc-duration">${ne}</span>`}
                ${v&&f`<span class="tc-status-badge tc-badge-fail">failed</span>`}
                ${b&&f`<span class="tc-status-badge tc-badge-cancelled">cancelled</span>`}
                ${y&&f`<span class="tc-status-icon">\u2713</span>`}
            </div>
            ${c.value&&f`
                <div class="tc-detail" ref=${m}
                     onClick=${e=>e.stopPropagation()}>
                    ${f`<${op} tool=${e} params=${t}
                        panelRef=${m} />`}
                    ${f`<${cp} tool=${e} params=${t}
                        result=${i} isFail=${v} isCancelled=${b}
                        showFull=${l} panelRef=${m} />`}
                </div>
            `}
        </div>
    `}function up({children:e,count:t}){return t<=1?e:f`
        <div class="tc-group">
            <div class="tc-group-label">${t} tools in parallel</div>
            ${e}
        </div>
    `}function dp(e,t){return e?e.length<=t?e:e.slice(0,t)+`...`:``}function fp(e){switch(e){case`system`:return`cd-role-system`;case`user`:return`cd-role-user`;case`assistant`:return`cd-role-assistant`;case`tool`:return`cd-role-tool`;default:return``}}function pp(e){return e==null?`--`:Number(e).toLocaleString()}function mp({msg:e,index:t}){let n=u(!1),r=e.role||`unknown`,i=e.content||``,a=e.tool_calls&&e.tool_calls.length>0,o=!!e.tool_call_id,s=dp(i,120),c=`[${t}] ${r}`;if(o&&(c+=` (tool_result)`),a){let t=e.tool_calls.map(e=>e.function?.name||`?`).join(`, `);c+=` -> ${t}`}return f`
        <div class="cd-msg" role="button" tabindex="0"
             onClick=${e=>{e.stopPropagation(),n.value=!n.value}}
             onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),e.stopPropagation(),n.value=!n.value)}}>
            <div class="cd-msg-header">
                <span class="cd-msg-chevron">${n.value?`▼`:`▶`}</span>
                <span class="cd-msg-role ${fp(r)}">${r}</span>
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
    `}function hp({messages:e,toolNames:t,totalTokens:n,systemTokens:r,historyMessageCount:i,agentName:a,agentId:o}){let s=u(!1),c=e=>{e.stopPropagation(),s.value=!s.value},l=Array.isArray(e)?e.length:0,d=a?`Context sent to LLM (${a})`:`Context sent to LLM`;return f`
        <div class="cd-row" role="button" tabindex="0"
             onClick=${c} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),c(e))}}>
            <div class="cd-header">
                <span class="cd-chevron">${s.value?`▼`:`▶`}</span>
                <span class="cd-icon">CTX</span>
                <span class="cd-title">${d}</span>
                <span class="cd-stats">
                    ${pp(n)} tokens | ${l} messages | ${(t||[]).length} tools
                </span>
            </div>
            ${s.value&&f`
                <div class="cd-detail" onClick=${e=>e.stopPropagation()}>
                    <!-- Token breakdown -->
                    <div class="cd-section">
                        <div class="cd-section-label">Token breakdown</div>
                        <div class="cd-token-grid">
                            <span class="cd-token-label">System prompt:</span>
                            <span class="cd-token-value">${pp(r)}</span>
                            <span class="cd-token-label">History messages:</span>
                            <span class="cd-token-value">${i}</span>
                            <span class="cd-token-label">Total estimated:</span>
                            <span class="cd-token-value cd-token-total">${pp(n)}</span>
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
                                <${mp} key=${t} msg=${e} index=${t} />
                            `)}
                        </div>
                    </div>
                </div>
            `}
        </div>
    `}async function gp(e,t){try{await m(`/approvals/${e}`,{decision:t})}catch(e){throw console.error(`[resolveApproval] failed:`,e),e}}function _p({approvalId:e,tool:t,params:n}){let r=u(!1),i=async()=>{if(!r.value){r.value=!0;try{await gp(e,`approve`)}catch{r.value=!1}}},a=async()=>{if(!r.value){r.value=!0;try{await gp(e,`deny`)}catch{r.value=!1}}},o=r.value;return f`
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
    `}function vp(e){return typeof e==`string`&&e.endsWith(`...`)}function yp({runId:e,summary:t,truncated:n}={}){return e?typeof n==`boolean`?n:vp(t):!1}function bp({jobSessionUuid:e}={}){return typeof e==`string`&&e.length>0?e:null}function xp(e,t){return typeof t==`string`&&t.length>0?t:e||``}var Sp=150;function Cp(e){if(!e)return``;try{return new Date(e).toLocaleTimeString(void 0,{hour:`2-digit`,minute:`2-digit`})}catch{return``}}function wp(e){switch(e){case`success`:return`Completed`;case`error`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Finished`}}function Tp(e){switch(e){case`success`:return`✓`;case`error`:return`✗`;case`cancelled`:return`–`;default:return`•`}}function Ep({jobName:e,status:t,summary:n,ts:r,runId:i,truncated:a,jobSessionUuid:o,jobSessionId:c}){let l=u(!1),d=u(null),m=u(!1),h=n&&n.length>Sp,g=!h||l.value;p(()=>{if(!l.value||d.value!==null||m.value||!yp({runId:i,summary:n,truncated:a}))return;m.value=!0;let e=!1;return Eu(i).then(t=>{e||(d.value=xp(n,t&&t.response))}).catch(()=>{e||(d.value=n||``)}).finally(()=>{e||(m.value=!1)}),()=>{e=!0}},[l.value,i,n,a]);let _=d.value==null?n:d.value,v=`job-card--${t||`success`}`,y=Cp(r),b=Tp(t),x=wp(t),ee=()=>{l.value=!l.value},S=bp({jobSessionUuid:o}),te=e=>{e.stopPropagation(),S&&Md(S,{logPrefix:`job-card`})},ne=_?s(_):``;return f`
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
                                ${n.slice(0,Sp)}...
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
    `}var Dp=200;function Op(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function kp(e){switch(e){case`done`:return`Completed`;case`fail`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Completed`}}function Ap(e){switch(e){case`done`:return`✓`;case`fail`:return`✗`;case`cancelled`:return`–`;default:return`✓`}}function jp({name:e,task:t,status:n,toolCount:r,durationMs:i,sessionId:a,summary:o}){let s=u(!1),c=`sa-card--${n||`done`}`,l=Ap(n),d=kp(n),p=Op(i),m=o&&o.length>Dp,h=!s.value&&m?o.slice(0,Dp)+`…`:o;return f`
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
                    <button class="sa-card-view-btn" onClick=${e=>{e.stopPropagation(),a&&fl(a)}}>
                        View session \u2192
                    </button>
                </div>
            `}
        </div>
    `}function Mp(e){let t=C.value.filter((t,n)=>n!==e);C.value=t,me(x.value,t)}function Np(){let e=C.value;return e.length===0?null:f`
        <div id="message-queue">
            ${e.map((e,t)=>f`
                <div class="queued-msg">
                    <span class="queued-msg-label">queued</span>
                    <span class="queued-msg-text">${e.text}</span>
                    <button class="queued-msg-remove" title="Remove from queue"
                            onClick=${()=>Mp(t)}>\u00d7</button>
                </div>
            `)}
        </div>
    `}var Pp=e({InputArea:()=>zp,startRun:()=>Fp});async function Fp(e,t){let n=t?.sessionId||x.value,r=T.value;if(!r){we(e=>[...e,{id:k(),type:`error`,text:`Select an agent before sending a message.`}]);return}j({id:k(),type:`user`,role:`user`,text:e,ts:new Date().toISOString()},{id:k(),type:`thinking`,pending:!0}),n&&ge(n,e);try{let t=await Tu({session_id:n,agent_id:r,input:{type:`text`,text:e}});n&&t?.run_id&&ne(n,t.run_id)}catch(e){n&&te(n),we(t=>[...t.filter(e=>e.type!==`thinking`),{id:k(),type:`error`,text:`Failed to start run: ${e.error?.message||e.message||e.status||`unknown error`}`}]),console.error(`[startRun] failed:`,e)}}function Ip(e){let t=e.current.value.trim();if(!t||!x.value||!T.value)return;let n=x.value;if(e.current.value=``,e.current.style.height=`auto`,de(n),O.value){let r=[...C.value,{text:t}];C.value=r,me(n,r),e.current.focus();return}Fp(t)}async function Lp(){if(O.value)try{await Ou(O.value)}catch{}}function Rp(e){e.style.height=`auto`,e.style.height=Math.min(e.scrollHeight,150)+`px`}function zp(){let e=n(null),t=E.value.length>0,r=!!x.value,i=!!T.value,a=t&&i&&r,o=!!O.value,s=i?`Send a message...`:`Select an agent to send a message`,c=x.value;return p(()=>{let t=e.current;t&&(t.value=pe(c),Rp(t));let n=oe(c),r=ee({restoredQueue:n,activeRunId:O.value,activeAgentId:T.value});n.length>0&&(C.value=n),r.drain&&(C.value=r.remaining,me(c,r.remaining),Fp(r.head.text,{sessionId:c}))},[c]),f`
        <div id="input-area">
            <div class="input-container">
                <textarea id="prompt" ref=${e} rows="1"
                          placeholder=${s}
                          aria-label="Message input"
                          disabled=${!a}
                          onKeyDown=${t=>{t.key===`Enter`&&!t.shiftKey&&(t.preventDefault(),Ip(e))}}
                          onInput=${()=>{let t=e.current;t&&(Rp(t),fe(c,t.value))}}></textarea>
                ${o?f`<button id="cancel-run" title="Stop run" aria-label="Stop run"
                                   onClick=${Lp}><${bd} /></button>`:f`<button id="send" disabled=${!a}
                                   title="Send (Enter)" aria-label="Send message"
                                   onClick=${()=>Ip(e)}><${yd} /></button>`}
            </div>
        </div>
    `}var Bp=()=>v(`/agents`),Vp=e=>m(`/agents`,e),Hp=(e,t)=>se(`/agents/${e}`,t),Up=e=>g(`/agents/${e}`),Wp=e=>m(`/agents/${e}/default`),Gp={"claude-opus-4-7":{name:`Claude Opus 4.7`,provider:`anthropic`},"claude-opus-4-6":{name:`Claude Opus 4.6`,provider:`anthropic`},"claude-sonnet-4-6":{name:`Claude Sonnet 4.6`,provider:`anthropic`},"claude-sonnet-4-5":{name:`Claude Sonnet 4.5`,provider:`anthropic`},"claude-haiku-4-5":{name:`Claude Haiku 4.5`,provider:`anthropic`},"gpt-5.4":{name:`GPT-5.4`,provider:`openai`},"gpt-5.4-mini":{name:`GPT-5.4 mini`,provider:`openai`},"gpt-5.4-nano":{name:`GPT-5.4 nano`,provider:`openai`},"gpt-4.1":{name:`GPT-4.1`,provider:`openai`},"gpt-4.1-mini":{name:`GPT-4.1 mini`,provider:`openai`},"gpt-4.1-nano":{name:`GPT-4.1 nano`,provider:`openai`},"gpt-4o":{name:`GPT-4o`,provider:`openai`},"gpt-4o-mini":{name:`GPT-4o mini`,provider:`openai`},"o4-mini":{name:`o4-mini`,provider:`openai`},o3:{name:`o3`,provider:`openai`},"o3-mini":{name:`o3-mini`,provider:`openai`},"grok-4.20":{name:`Grok 4.20`,provider:`xai`},"grok-4-fast":{name:`Grok 4 Fast`,provider:`xai`},"grok-3":{name:`Grok 3`,provider:`xai`},"grok-3-mini":{name:`Grok 3 mini`,provider:`xai`},"deepseek-chat":{name:`DeepSeek Chat (V3)`,provider:`deepseek`},"deepseek-reasoner":{name:`DeepSeek Reasoner (R1)`,provider:`deepseek`},"mistral-large-latest":{name:`Mistral Large`,provider:`mistral`},"mistral-medium-latest":{name:`Mistral Medium`,provider:`mistral`},"mistral-small-latest":{name:`Mistral Small`,provider:`mistral`},"codestral-latest":{name:`Codestral`,provider:`mistral`},"ministral-8b-latest":{name:`Ministral 8B`,provider:`mistral`},"open-mistral-nemo":{name:`Mistral Nemo`,provider:`mistral`},"llama-3.3-70b-versatile":{name:`Llama 3.3 70B`,provider:`groq`},"llama-3.1-8b-instant":{name:`Llama 3.1 8B (Instant)`,provider:`groq`},"deepseek-r1-distill-llama-70b":{name:`DeepSeek R1 Distill (70B)`,provider:`groq`},"qwen-2.5-32b":{name:`Qwen 2.5 32B`,provider:`groq`},"qwen2.5-coder:32b":{name:`Qwen 2.5 Coder 32B`,provider:`ollama`},"deepseek-r1:7b":{name:`DeepSeek R1 7B`,provider:`ollama`},"llama3.3:70b":{name:`Llama 3.3 70B`,provider:`ollama`},"deepseek/deepseek-r1":{name:`DeepSeek R1`,provider:`openrouter`},"deepseek/deepseek-chat-v3-0324":{name:`DeepSeek Chat v3`,provider:`openrouter`},"z-ai/glm-5.2":{name:`GLM 5.2`,provider:`openrouter`},"z-ai/glm-5.1":{name:`GLM 5.1`,provider:`openrouter`},"minimax/minimax-m2.7":{name:`MiniMax M2.7`,provider:`openrouter`},"xiaomi/mimo-v2-pro":{name:`MiMo v2-pro`,provider:`openrouter`},"moonshotai/kimi-k2.6":{name:`Kimi K2.6`,provider:`openrouter`},"google/gemma-4-31b-it":{name:`Gemma 4 31B`,provider:`openrouter`}},Kp=`claude-opus-4-7,claude-sonnet-4-6,claude-haiku-4-5,claude-opus-4-6,gpt-5.4,gpt-5.4-mini,gpt-5.4-nano,gpt-4.1,gpt-4.1-mini,gpt-4o,gpt-4o-mini,o4-mini,o3,grok-4.20,grok-4-fast,grok-3-mini,deepseek-chat,deepseek-reasoner,mistral-large-latest,mistral-small-latest,codestral-latest,llama-3.3-70b-versatile,llama-3.1-8b-instant,deepseek-r1-distill-llama-70b,qwen2.5-coder:32b,deepseek-r1:7b,llama3.3:70b,z-ai/glm-5.2,deepseek/deepseek-r1,deepseek/deepseek-chat-v3-0324,z-ai/glm-5.1,minimax/minimax-m2.7,xiaomi/mimo-v2-pro,moonshotai/kimi-k2.6,google/gemma-4-31b-it`.split(`,`);function qp(e){if(!e)return``;let t=Gp[e];return t?t.name:e}function Jp(e){if(!e)return`unknown`;let t=Gp[e];return t?t.provider:e.includes(`/`)?`openrouter`:e.includes(`:`)?`ollama`:e.startsWith(`claude`)?`anthropic`:e.startsWith(`gpt`)||/^o\d/.test(e)?`openai`:e.startsWith(`grok`)?`xai`:e.startsWith(`deepseek-`)?`deepseek`:e.startsWith(`mistral-`)||e.startsWith(`codestral-`)||e.startsWith(`ministral-`)||e.startsWith(`open-mistral-`)||e.startsWith(`open-mixtral-`)?`mistral`:e.startsWith(`llama-`)?`groq`:e.startsWith(`gemini-`)?`google`:`unknown`}var Yp={anthropic:`Anthropic`,openai:`OpenAI`,openrouter:`OpenRouter`,xai:`xAI`,deepseek:`DeepSeek`,mistral:`Mistral`,groq:`Groq`,ollama:`Ollama`,google:`Google`,unknown:`Custom`};function Xp(e){return e?Yp[e]?Yp[e]:Yp[Jp(e)]:Yp.unknown}function Zp({modelId:e,provider:t}){let n=t||Jp(e),r=Yp[n]||Yp.unknown;return f`
        <span class="model-provider-badge model-provider-badge--${n}"
              title=${`Provider: ${r}`}>${r}</span>
    `}function Qp({value:e,defaultValue:t,showBadge:n=!0}){let r=e&&e.trim?e.trim():e,i=!!r&&r!==t,a=r||t;if(!a)return f`<span class="model-display model-display--muted">unknown</span>`;let o=qp(a),s=o===a?a:`${o} (${a})`;return i?f`
            <span class="model-display" title=${s}>
                <span class="model-override-pill" title="Per-run override">override</span>
                <span class="model-name">${o}</span>
                ${n&&f`<${Zp} modelId=${a} />`}
            </span>
        `:f`
        <span class="model-display model-display--default" title=${s}>
            <span class="model-default-label">Default</span>
            <span class="model-name">${o}</span>
            ${n&&f`<${Zp} modelId=${a} />`}
        </span>
    `}function $p({value:e,defaultValue:t}){let n=e&&e.trim?e.trim():e,r=!!n&&n!==t,i=n||t;return i?r?f`
            <span class="model-display">
                <span class="model-override-pill" title="Per-run override">override</span>
                <${Zp} provider=${i} />
            </span>
        `:f`
        <span class="model-display model-display--default">
            <span class="model-default-label">Default</span>
            <${Zp} provider=${i} />
        </span>
    `:f`<span class="model-display model-display--muted">unknown</span>`}function em(e){return e==null?``:String(e).toLowerCase().replace(/\s+/g,`-`).replace(/[^a-z0-9-]/g,``).replace(/-+/g,`-`).replace(/^-+|-+$/g,``)}var tm=[`default`,`dm`,`workspace`],nm=/^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32})$/,rm=64;function im(e){return typeof e!=`string`||e.length===0?null:e.length>rm?{code:`AGENT_NAME_TOO_LONG`,message:`Agent name is too long (max ${rm} characters after normalization)`}:tm.includes(e)?{code:`AGENT_NAME_RESERVED`,message:`'${e}' is a reserved name`}:nm.test(e)?{code:`AGENT_NAME_LOOKS_LIKE_UUID`,message:`'${e}' looks like a UUID (conflicts with ID-based lookup)`}:null}function am(e){return e===`Enter`||e===` `}function om(e){return!e||typeof e.key!=`string`||e.defaultPrevented?!1:am(e.key)}function sm(e){return!(!e||e.defaultPrevented)}function cm(e,t){let n=!!e,r=!!t;return n===r?{}:{debug_mode:r}}var lm=[`minimal`,`low`,`medium`,`high`];function um(e){return e==null?`inherit`:e===0?`disable`:`custom`}function dm(e){return e==null||e===``?`inherit`:`custom`}function fm({label:e,hint:t,agentValue:n,mode:r,draft:i}){return f`
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
    `}function pm({label:e,hint:t,mode:n,value:r}){return f`
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
                        ${lm.map(e=>f`<option value=${e} key=${e}>${e}</option>`)}
                    </select>
                `}
            </div>
            ${t&&f`<span class="settings-hint">${t}</span>`}
        </div>
    `}async function mm(){try{let e=await Bp();E.value=e.agents||e||[]}catch(e){console.error(`[agents] fetch failed:`,e)}}function hm({agent:e,onClose:t}){let n=u(e.description||``),r=u(e.model||``),i=u(e.posture||``),a=u(e.provider||``),o=u(`keep`),s=u(``),c=u(um(e.thinking_budget_tokens)),l=u(e.thinking_budget_tokens&&e.thinking_budget_tokens>0?String(e.thinking_budget_tokens):``),d=u(dm(e.reasoning_effort)),p=u(e.reasoning_effort||``),m=u(um(e.gemini_thinking_budget)),h=u(e.gemini_thinking_budget&&e.gemini_thinking_budget>0?String(e.gemini_thinking_budget):``),g=u(e.summary_provider||``),_=u(e.summary_model||``),v=u(!!e.debug_mode),y=u(!1),b=u(``),x=_c.value.model||``,ee=_c.value.provider||``,S=_c.value.llm_providers||[],te=()=>{let t={};n.value!==(e.description||``)&&(t.description=n.value),(r.value||``)!==(e.model||``)&&(t.model=r.value||``),(i.value||``)!==(e.posture||``)&&(t.posture=i.value||``),(a.value||``)!==(e.provider||``)&&(t.provider=a.value||``),o.value===`set`&&s.value.trim()?t.telegram_token=s.value.trim():o.value===`remove`&&(t.telegram_token=``);let u=e.thinking_budget_tokens;if(c.value===`inherit`)u!=null&&(t.clear_thinking_budget_tokens=!0);else if(c.value===`disable`)u!==0&&(t.thinking_budget_tokens=0);else if(c.value===`custom`){let e=parseInt(l.value,10);!isNaN(e)&&e>=0&&e!==u&&(t.thinking_budget_tokens=e)}let f=e.reasoning_effort||null;d.value===`inherit`?f!=null&&(t.clear_reasoning_effort=!0):d.value===`custom`&&p.value&&p.value!==f&&(t.reasoning_effort=p.value);let y=e.gemini_thinking_budget;if(m.value===`inherit`)y!=null&&(t.clear_gemini_thinking_budget=!0);else if(m.value===`disable`)y!==0&&(t.gemini_thinking_budget=0);else if(m.value===`custom`){let e=parseInt(h.value,10);!isNaN(e)&&e>=0&&e!==y&&(t.gemini_thinking_budget=e)}let b=e.summary_provider||``,x=(g.value||``).trim();x!==b&&(x===``?t.clear_summary_provider=!0:t.summary_provider=x);let ee=e.summary_model||``,S=(_.value||``).trim();return S!==ee&&(S===``?t.clear_summary_model=!0:t.summary_model=S),Object.assign(t,cm(e.debug_mode,v.value)),t},ne=async()=>{y.value=!0,b.value=``;try{let n=te();if(Object.keys(n).length===0){t();return}await Hp(e.id,n),await mm(),t()}catch(e){b.value=e.error?.message||e.message||`Save failed`}finally{y.value=!1}},re=e=>{e.target===e.currentTarget&&t()},ie=!!e.has_telegram;return f`
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
                        ${Kp.map(e=>f`<option value=${e} />`)}
                    </datalist>
                    <span class="settings-effective">
                        Effective: <${Qp} value=${r.value.trim()} defaultValue=${x} />
                    </span>
                    <span class="settings-hint">Leave empty to use server default.</span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Provider</label>
                    <select class="settings-select"
                            value=${a.value}
                            onChange=${e=>{a.value=e.target.value}}>
                        <option value="">Default (${Xp(ee||`openai`)})</option>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="openrouter">OpenRouter</option>
                    </select>
                    <span class="settings-effective">
                        Effective: <${$p} value=${a.value} defaultValue=${ee||`openai`} />
                    </span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Posture</label>
                    <select class="settings-select"
                            value=${i.value}
                            onChange=${e=>{i.value=e.target.value}}>
                        <option value="">Server default (${_c.value.posture||`guarded`})</option>
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

                <${fm}
                    label="Anthropic thinking budget"
                    hint="Inherit = use server default. Disable = Some(0) (force off for this agent). Custom = override with N tokens."
                    agentValue=${e.thinking_budget_tokens}
                    mode=${c}
                    draft=${l} />

                <${pm}
                    label="OpenAI reasoning effort"
                    hint="Inherit = use server default. Custom picks an effort level for this agent."
                    mode=${d}
                    value=${p} />

                <${fm}
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
                        ${(S.length>0?S:[`openai`,`anthropic`,`openrouter`,`gemini`]).map(e=>{let t=Xp(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
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
                            ${ie?`configured (token hidden)`:`not configured`}
                        </span>
                        <select class="settings-select agent-tristate-mode"
                                value=${o.value}
                                onChange=${e=>{o.value=e.target.value,e.target.value!==`set`&&(s.value=``)}}>
                            <option value="keep">Keep</option>
                            <option value="set">${ie?`Replace`:`Set`}</option>
                            ${ie&&f`<option value="remove">Remove</option>`}
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
                        ${ie?`A token is set but is never displayed. Replace overwrites it; Remove clears it.`:`Set a bot token to enable a dedicated Telegram polling loop for this agent.`}
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
    `}function gm({agent:e,isActive:t,onEdit:n}){let r=u(``),i=u(!1),a=u(null),o=_c.value.model||``,s=_c.value.provider||``;return f`
        <div class="agent-card ${t?`active`:``}"
             role="option"
             tabindex="0"
             aria-label=${`Select agent `+e.name}
             aria-selected=${t?`true`:`false`}
             onClick=${t=>{sm(t)&&ld(e.id)}}
             onKeyDown=${t=>{om(t)&&(t.preventDefault(),ld(e.id))}}>
            <div class="agent-card-header">
                <span class="agent-card-name">${e.name}</span>
                ${e.is_default&&f`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta agent-card-meta--model">
                <span class="agent-card-meta-label">model:</span>
                <${Qp} value=${e.model} defaultValue=${o} />
            </div>
            ${e.provider&&f`
                <div class="agent-card-meta agent-card-meta--provider">
                    <span class="agent-card-meta-label">provider:</span>
                    <${$p} value=${e.provider} defaultValue=${s} />
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
                    <button class="agent-card-btn" onClick=${async t=>{t&&t.stopPropagation();try{await Wp(e.id),await mm()}catch(e){r.value=e.error?.message||e.message||`Failed`}}}>Set Default</button>
                `}
                ${i.value?f`
                        <button class="agent-card-btn" style="color:var(--error); font-weight:600;" onClick=${async t=>{t&&t.stopPropagation(),a.value&&=(clearTimeout(a.value),null),i.value=!1;try{if(await Up(e.id),await mm(),e.id===T.value){let e=E.value.find(e=>e.is_default)||E.value[0]||null;e?ld(e.id):(T.value=null,ye.value=null)}}catch(e){r.value=e.error?.message||e.message||`Delete failed`}}}>Confirm?</button>
                        <button class="agent-card-btn" onClick=${e=>{e&&e.stopPropagation(),a.value&&=(clearTimeout(a.value),null),i.value=!1}}>Cancel</button>
                    `:f`<button class="agent-card-btn" style="color:var(--error);" onClick=${e=>{e&&e.stopPropagation(),i.value=!0,a.value=setTimeout(()=>{i.value=!1},3e3)}}>Delete</button>`}
            </div>
        </div>
    `}function _m(){let e=u(``),t=u(``),n=u(!1),r=u(null);p(()=>{Z.value===`agents`&&mm()},[Z.value]);let i=em(e.value),a=(e.value||``).trim(),o=a!==``&&i!==a,s=async()=>{let r=em(e.value);if(!r){(e.value||``).trim()===``?t.value=`Agent name is required`:t.value=`Agent name must contain at least one letter or digit`;return}let i=im(r);if(i){t.value=i.message;return}t.value=``,n.value=!0;try{let t=await Vp({name:r});t.id||console.warn(`[agents] POST /agents returned no id for agent:`,r,t),e.value=``,await mm()}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}};return f`
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
                ${E.value.length===0?f`<div class="empty-state">No agents</div>`:E.value.map(e=>f`
                        <${gm} key=${e.id} agent=${e}
                                      isActive=${e.id===T.value}
                                      onEdit=${e=>{r.value=e}} />
                    `)}
            </div>

            ${r.value&&f`
                <${hm}
                    agent=${r.value}
                    onClose=${()=>{r.value=null}} />
            `}
        </div>
    `}var vm=e=>v(`/agents/${e}/workspace`),ym=(e,t,n)=>se(`/agents/${e}/workspace/${t}`,{content:n}),bm=e=>m(`/agents/${e}/workspace/open`,{}),xm=[`personality`,`goals`,`memories`,`user`];async function Sm(){if(!T.value){yc.value=null;return}try{let e=await vm(T.value);yc.value=e.files||e}catch(e){e.status===404||e.error?.code===`NOT_FOUND`?yc.value=`unavailable`:yc.value=`error`}}function Cm({agentId:e,doOpen:t}){let n=u(!1),r=u(null),i=async()=>{if(!(n.value||!e)){n.value=!0,r.value=null;try{await t(e),r.value={kind:`ok`,text:`Opened`},setTimeout(()=>{r.value?.kind===`ok`&&(r.value=null)},2e3)}catch(e){let t=e?.error?.code,n=e?.error?.message||e?.message||`Failed to open workspace`,i=n;t===`NOT_CONFIGURED`?i=`Workspace dir not configured`:t===`WORKSPACE_PATH_MISSING`?i=`Workspace path is missing on disk`:t===`LAUNCHER_FAILED`&&(i=`Failed to launch file explorer`),r.value={kind:`err`,text:i,full:n}}finally{n.value=!1}}};return f`
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
    `}function wm({agentId:e,filename:t,content:n}){let r=u(n||``),i=u(``),a=u(!1);return p(()=>{r.value=n||``},[n]),f`
        <div class="ws-file">
            <div class="ws-file-label">${t}</div>
            <textarea class="ws-textarea"
                      rows="6"
                      value=${r.value}
                      onInput=${e=>{r.value=e.target.value}}></textarea>
            <div style="display:flex; align-items:center; gap:var(--space-2);">
                <button class="ws-save" onClick=${async()=>{if(!a.value){a.value=!0,i.value=``;try{await ym(e,t,r.value),i.value=`Saved`,setTimeout(()=>{i.value=``},2e3),await Sm()}catch(e){i.value=`Error: `+(e.error?.message||e.message||`save failed`)}finally{a.value=!1}}}} disabled=${a.value}>
                    ${a.value?`Saving...`:`Save`}
                </button>
                ${i.value&&f`
                    <span class="ws-flash ${i.value.startsWith(`Error`)?`err`:`ok`}">
                        ${i.value}
                    </span>
                `}
            </div>
        </div>
    `}function Tm(){return p(()=>{Z.value===`workspace`&&Sm()},[Z.value,T.value]),T.value?yc.value===null?f`<div class="loading-state">Loading...</div>`:yc.value===`unavailable`?f`<div class="ws-notice">Workspace not configured for this agent</div>`:yc.value===`error`?f`<div class="ws-notice" style="color:var(--error);">Failed to load workspace</div>`:f`
        <div>
            <${Cm}
                agentId=${T.value}
                doOpen=${bm} />
            ${xm.map(e=>f`
                <${wm}
                    key=${e}
                    agentId=${T.value}
                    filename=${e}
                    content=${yc.value[e+`.md`]||yc.value[e]||``} />
            `)}
        </div>
    `:f`<div class="ws-notice">No agent selected</div>`}var Em=d([]),Dm=()=>v(`/jobs`),Om=e=>m(`/jobs`,e),km=e=>g(`/jobs/${e}`),Am=[{label:`1m`,cron:`* * * * *`,desc:`Every minute`},{label:`5m`,cron:`*/5 * * * *`,desc:`Every 5 minutes`},{label:`15m`,cron:`*/15 * * * *`,desc:`Every 15 minutes`},{label:`30m`,cron:`*/30 * * * *`,desc:`Every 30 minutes`},{label:`1h`,cron:`0 * * * *`,desc:`Every hour`},{label:`6h`,cron:`0 */6 * * *`,desc:`Every 6 hours`},{label:`12h`,cron:`0 */12 * * *`,desc:`Every 12 hours`},{label:`1d`,cron:`0 0 * * *`,desc:`Daily at midnight`}];function jm(e){if(!e)return``;let t=Am.find(t=>t.cron===e.trim());return t?t.desc:e.trim().split(/\s+/).length===5?e:`Invalid cron (need 5 fields)`}function Mm(e){let t=e=>String(e).padStart(2,`0`);return`${e.getFullYear()}-${t(e.getMonth()+1)}-${t(e.getDate())}T${t(e.getHours())}:${t(e.getMinutes())}`}function Nm(){let e=new Date(Date.now()+5*6e4);return e.setSeconds(0,0),Mm(e)}function Pm(){let e=new Date;return e.setSeconds(0,0),Mm(e)}async function Fm(){try{let e=await Dm();Em.value=e.jobs||e||[]}catch(e){console.error(`[jobs] fetch failed:`,e)}}function Im(){let e=u(`recurring`),t=u(``),n=u(Nm()),r=u(``),i=u(T.value||``),a=u(``),o=u(``),s=u(!1),c=u(!1);p(()=>{Z.value===`jobs`&&Fm()},[Z.value]),p(()=>{i.value=T.value||``},[T.value]);let l=jm(t.value),d=e.value===`once`?!!n.value:!!t.value.trim(),m=!!i.value&&d&&!!r.value.trim(),h=async()=>{if(m){a.value=``,o.value=``,s.value=!0;try{let l;if(e.value===`once`){let e=new Date(n.value);if(isNaN(e.getTime())){a.value=`Invalid date/time. Please select a valid date.`,s.value=!1;return}l={type:`once`,run_at:e.toISOString()}}else l={type:`recurring`,cron:t.value.trim()};await Om({agent_id:i.value,schedule:l,prompt:r.value.trim()}),t.value=``,r.value=``,c.value=!1,n.value=Nm(),o.value=e.value===`once`?`Job scheduled (one-time).`:`Recurring job created.`,setTimeout(()=>{o.value=``},4e3),await Fm()}catch(e){let t=e.error?.message||e.message||``;a.value=t||`Failed to create job. Check that all fields are filled and the schedule is valid.`}finally{s.value=!1}}},g=async e=>{try{await km(e),await Fm()}catch(e){a.value=e.error?.message||e.message||`Failed to cancel job`}};return f`
        <div>
            <div class="jobs-form">
                <select class="jobs-select" value=${i.value}
                        onChange=${e=>{i.value=e.target.value}}>
                    ${E.value.map(e=>f`
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
                        ${Am.map(e=>f`
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
                           min=${Pm()}
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

            ${Em.value.length===0?f`<div class="jobs-empty">No scheduled jobs</div>`:Em.value.map(e=>f`
                    <div class="job-item">
                        <div class="job-prompt">${e.prompt||e.task||`(no prompt)`}</div>
                        <div class="job-meta">
                            <span>${jm(e.schedule?.cron)||(e.schedule?.type===`once`?`Once at `+Qd(e.schedule.run_at):JSON.stringify(e.schedule))}</span>
                            ${e.next_run_at&&f`<span> | next: ${Qd(e.next_run_at)}</span>`}
                            ${e.last_run_at&&f`<span> | last run: ${Qd(e.last_run_at)}</span>`}
                        </div>
                        <span class="job-status-${e.status||`active`}">${e.status||`active`}</span>
                        ${e.status!==`cancelled`&&f`
                            <button class="job-cancel" onClick=${()=>g(e.id)}>Cancel</button>
                        `}
                    </div>
                `)}
        </div>
    `}var Lm=(e,t=50)=>v(`/audit?session_id=${e}&limit=${t}`),Rm=50;async function zm(e){if(!x.value){bc.value=null;return}try{let t=await Lm(x.value,e);bc.value=t.events||t||[]}catch{bc.value=[]}}function Bm(){let e=u(Rm),t=u(!1);p(()=>{Z.value===`audit`&&(e.value=Rm,zm(Rm))},[Z.value,x.value]);let n=async()=>{t.value=!0;try{let t=e.value+Rm;e.value=t,await zm(t)}catch(e){console.error(`[AuditTab] loadMore failed:`,e)}finally{t.value=!1}};if(!x.value)return f`<div class="empty-state">No session selected</div>`;if(bc.value===null)return f`<div class="loading-state">Loading...</div>`;if(bc.value.length===0)return f`<div class="empty-state">No audit events</div>`;let r=bc.value.length>=e.value;return f`
        <div>
            ${bc.value.map((e,t)=>f`
                <div class="audit-event" key=${e.id||`audit-${e.timestamp||``}-${t}`}>
                    <span class="audit-tool">${e.tool||e.action||`unknown`}</span>
                    <span class="${e.decision===`deny`?`audit-deny`:e.decision===`error`?`audit-error`:`audit-allow`}">
                        ${e.decision===`deny`?`denied`:e.decision===`error`?`error`:`allowed`}
                    </span>
                    ${e.timestamp&&f`<span class="audit-time">${Qd(e.timestamp)}</span>`}
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
    `}var Vm=50,Hm={completed:`✓`,failed:`✗`,cancelled:`⊘`,running:`⋯`},Um={user:`user`,scheduled:`scheduled`,subagent:`subagent`,dm:`dm`,notification:`notif`,telegram:`telegram`},Wm={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`};function Gm(e){if(e==null)return`--`;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m`+(n>0?n+`s`:``)}function Km(e){return e==null?`--`:e>=1e4?(e/1e3).toFixed(0)+`k`:e>=1e3?(e/1e3).toFixed(1)+`k`:String(e)}function qm(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function Jm(){let e=u([]),t=u(!1),n=u(``),r=async()=>{if(!T.value){e.value=[];return}t.value=!0,n.value=``;try{let t=await Mu(T.value,Vm);e.value=t.runs||[]}catch(t){console.error(`[RunsTab] fetch failed:`,t),n.value=t.error?.message||t.message||`Failed to load runs`,e.value=[]}finally{t.value=!1}};return p(()=>{Z.value===`runs`&&r()},[Z.value,T.value,Ce.value]),T.value?t.value&&e.value.length===0?f`<div class="loading-state">Loading runs...</div>`:n.value?f`
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
                         onClick=${()=>e.session_id&&Md(e.session_id)}
                         title=${`Run `+e.run_id.slice(0,8)+` | Session `+(e.session_id||``).slice(0,8)}>
                        <div class="runs-tab-row-top">
                            <span class="runs-tab-status">${Hm[e.status]||`·`}</span>
                            <span class="runs-tab-trigger runs-tab-trigger--${e.trigger||`user`}">
                                ${Um[e.trigger]||e.trigger||`user`}
                            </span>
                            <span class="runs-tab-session-type">
                                ${Wm[e.session_type]||e.session_type||``}
                            </span>
                            <span class="runs-tab-time">${qm(e.ts)}</span>
                        </div>
                        <div class="runs-tab-row-bottom">
                            <span class="runs-tab-duration">${Gm(e.duration_ms)}</span>
                            <span class="runs-tab-tools">${e.tool_call_count==null?``:e.tool_call_count+` tools`}</span>
                            <span class="runs-tab-tokens">
                                ${e.usage?Km(e.usage.prompt_tokens)+` in / `+Km(e.usage.completion_tokens)+` out`+(typeof e.usage.reasoning_tokens==`number`&&e.usage.reasoning_tokens>0?` (+`+Km(e.usage.reasoning_tokens)+` reasoning)`:``)+(typeof e.usage.cache_read_input_tokens==`number`&&e.usage.cache_read_input_tokens>0?` (`+Km(e.usage.cache_read_input_tokens)+` cached)`:``):``}
                            </span>
                        </div>
                    </div>
                `)}
            </div>
        </div>
    `:f`<div class="runs-tab-empty">No agent selected</div>`}function Ym(e,t=50,n=null){let r=`/agents/${e}/timeline?limit=${t}`;return n&&(r+=`&before=${encodeURIComponent(n)}`),v(r)}var Xm=50,Zm={run_started:`▶`,run_completed:`✓`,run_failed:`✗`,run_cancelled:`⊘`,run_ended:`■`,tool_call:`⚙`,message_received:`●`,message_sent:`○`,marker:`⚑`},Qm={run_started:`started`,run_completed:`completed`,run_failed:`failed`,run_cancelled:`cancelled`,run_ended:`ended`,tool_call:`tool`,message_received:`message`,message_sent:`sent`,marker:`marker`},$m={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`,episodic:`epis`};function eh(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function th(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function nh(e){if(!e)return``;let t=new Date(e),n=new Date,r=new Date;return r.setDate(r.getDate()-1),t.toDateString()===n.toDateString()?`Today`:t.toDateString()===r.toDateString()?`Yesterday`:t.toLocaleDateString([],{weekday:`short`,month:`short`,day:`numeric`})}async function rh(e){if(!e||e===x.value)return;let t=ke();fu(),c(()=>{x.value=e,O.value=null,Se.value=null,Te([]),C.value=[],bc.value=null,ml(),qc.value=null,Sc.value=!0}),rd(T.value,e);try{await Uu(e,{isStale:()=>t!==Oe,logPrefix:`timelineTab`})}finally{t===Oe&&(Sc.value=!1)}}function ih(){let e=u([]),t=u(!1),n=u(!1),r=u(``),i=u(!1),a=u(null),o=async(o=!1)=>{if(!T.value){e.value=[];return}o?n.value=!0:t.value=!0,r.value=``;try{let t=o?a.value:null,n=await Ym(T.value,Xm,t),r=n.events||[];o?e.value=[...e.value,...r]:e.value=r,i.value=n.pagination?.has_more||!1,a.value=n.pagination?.next_before||null}catch(t){console.error(`[TimelineTab] fetch failed:`,t),r.value=t.error?.message||t.message||`Failed to load timeline`,o||(e.value=[])}finally{t.value=!1,n.value=!1}};if(p(()=>{Z.value===`timeline`&&o(!1)},[Z.value,T.value]),!T.value)return f`<div class="tl-empty">No agent selected</div>`;if(t.value&&e.value.length===0)return f`<div class="loading-state">Loading timeline...</div>`;if(r.value)return f`
            <div>
                <div class="tl-error">${r.value}</div>
                <button class="tl-retry" onClick=${()=>o(!1)}>Retry</button>
            </div>
        `;if(e.value.length===0)return f`<div class="tl-empty">No activity yet</div>`;let s=new Set;{let t=``;for(let n of e.value){let e=nh(n.timestamp);e!==t&&(s.add(n),t=e)}}return f`
        <div class="tl-tab">
            <div class="tl-header">
                <span class="tl-count">${e.value.length} event${e.value.length===1?``:`s`}</span>
                <button class="tl-refresh" onClick=${()=>o(!1)}
                        disabled=${t.value} title="Refresh">
                    ${t.value?`...`:`↻`}
                </button>
            </div>
            <div class="tl-list">
                ${e.value.map((e,t)=>{let n=nh(e.timestamp),r=s.has(e),i=e.event_type===`tool_call`,a=e.event_type===`run_started`||e.event_type===`run_completed`||e.event_type===`run_failed`||e.event_type===`run_cancelled`||e.event_type===`run_ended`,o=e.metadata?.tool_name,c=e.event_type+`-`+e.timestamp+`-`+(e.run_id||``)+`-`+t+(o?`-`+o:``);return f`
                        ${r&&f`
                            <div class="tl-date-group" key=${`g-`+n}>${n}</div>
                        `}
                        <div class="tl-event tl-event--${e.event_type}${i?` tl-event--indent`:``}${a?` tl-event--run`:``}"
                             key=${c}
                             onClick=${()=>rh(e.session_id)}
                             title=${`Session `+(e.session_id||``).slice(0,8)+(e.run_id?` | Run `+e.run_id.slice(0,8):``)}>
                            <span class="tl-time">${th(e.timestamp)}</span>
                            <span class="tl-icon tl-icon--${e.event_type}">${Zm[e.event_type]||`·`}</span>
                            <span class="tl-session-badge tl-session-badge--${e.session_type||`chat`}">
                                ${$m[e.session_type]||e.session_type||`chat`}
                            </span>
                            <span class="tl-event-label">${Qm[e.event_type]||e.event_type}</span>
                            <span class="tl-ago">${eh(e.timestamp)}</span>
                        </div>
                        ${e.summary&&f`
                            <div class="tl-summary${i?` tl-summary--indent`:``}"
                                 onClick=${()=>rh(e.session_id)}>
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
    `}function ah({tab:e}){return e===`agents`?f`<${_m} />`:e===`workspace`?f`<${Tm} />`:e===`runs`?f`<${Jm} />`:e===`jobs`?f`<${Im} />`:e===`audit`?f`<${Bm} />`:e===`timeline`?f`<${ih} />`:null}function oh(){ud.value=null}function sh(){return ud.value?f`
        <div id="panel" class="open">
            <div class="panel-header">
                <span class="panel-header-title">${Z.value.charAt(0).toUpperCase()+Z.value.slice(1)}</span>
                <button class="panel-close-btn" title="Close panel" aria-label="Close panel"
                        onClick=${oh}>\u00D7</button>
            </div>
            <div class="panel-body">
                <${ah} tab=${Z.value} />
            </div>
        </div>
    `:null}var ch=()=>v(`/auth/keys`),lh=(e,t)=>se(`/auth/keys`,{provider:e,key:t}),uh=e=>g(`/auth/keys/${e}`),dh=[`openai`,`anthropic`,`openrouter`,`gemini`];function fh({title:e,defaultOpen:t=!1,children:n}){let r=u(t);return f`
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
    `}function ph({label:e,value:t,desc:n}){return f`
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
    `}function mh(){let e=u([]),t=u(null),n=u(``),r=u(!1),i=u(``),a=async()=>{try{let t=await ch();e.value=t.keys||[]}catch(e){console.error(`[auth] list keys failed:`,e)}};p(()=>{a()},[]);let o=async e=>{if(n.value.trim()){r.value=!0,i.value=``;try{await lh(e,n.value.trim()),n.value=``,t.value=null,await a()}catch(e){i.value=e.error?.message||e.message||`Failed to save key`}finally{r.value=!1}}},s=async e=>{try{await uh(e),await a()}catch(e){i.value=e.error?.message||e.message||`Failed to remove key`}};return f`
        <div class="settings-row">
            <label class="settings-label">API Keys</label>
            ${dh.map(i=>{let a=e.value.find(e=>e.provider===i),c=a?.configured,l=a?.source||`none`,u=a?.key||``;return t.value===i?f`
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
    `}function hh({open:e,onClose:t}){let n=u(!1),r=u(!1),i=u(``),a=u(``),o=u(``),s=u(``),c=u(``),l=u(``),d=u(``),m=u(``),h=u(``),g=u(!0),_=u(``),v=u(``),y=u(``),b=u(``),x=u(``),ee=u(``),S=u(``),te=u(``),ne=u(!0),re=u(!1),ie=u(``),ae=u(``),oe=u(!0),se=u(!1),ce=u(``),le=u(!1),ue=u(!1),de=u(!1),fe=u(``);if(p(()=>{if(e){let e=_c.value,t=e.context||{},u=e.session||{},f=e.tools||{},p=e.llm||{},de=p.anthropic||{},C=p.openai||{},pe=p.gemini||{};i.value=t.strategy||`truncate`,a.value=t.max_input_tokens==null?``:String(t.max_input_tokens),o.value=t.compact_trigger_pct==null?``:String(t.compact_trigger_pct),s.value=t.compact_retain_pct==null?``:String(t.compact_retain_pct),c.value=t.summary_model||``,l.value=t.summary_provider||``,d.value=u.max_messages==null?``:String(u.max_messages),m.value=u.max_context_tokens==null?``:String(u.max_context_tokens),h.value=u.idle_timeout_secs==null?``:String(u.idle_timeout_secs),g.value=u.auto_archive==null||u.auto_archive,_.value=u.archive_ttl_secs==null?``:String(u.archive_ttl_secs),v.value=f.shell_policy||`sandboxed`,y.value=f.sandbox_root||`.`,b.value=f.timeout_secs==null?``:String(f.timeout_secs),x.value=f.max_output_bytes==null?``:String(f.max_output_bytes),ee.value=e.model||``,S.value=e.provider||``,te.value=de.thinking_budget_tokens==null?``:String(de.thinking_budget_tokens),ne.value=de.prompt_cache_enabled==null||!!de.prompt_cache_enabled,re.value=!1,ie.value=C.reasoning_effort||``,ae.value=pe.thinking_budget==null?``:String(pe.thinking_budget),oe.value=pe.cache_enabled==null||!!pe.cache_enabled,se.value=!1,ce.value=pe.cache_ttl_seconds==null?``:String(pe.cache_ttl_seconds);let me=E.value.find(e=>e.id===T.value);le.value=!!(me&&me.debug_mode),ue.value=!1,n.value=!1,r.value=!1,fe.value=``}},[e]),!e)return null;let C=_c.value,pe=C.context||{},me=C.session||{},he=C.logging||{},ge=C.tools||{},_e=C.llm||{},ve=_e.anthropic||{},ye=_e.openai||{},w=_e.gemini||{},D=async()=>{de.value=!0,fe.value=``,n.value=!1;let e={},u={};i.value&&i.value!==(pe.strategy||``)&&(u.strategy=i.value);let f=parseInt(a.value,10);!isNaN(f)&&f!==pe.max_input_tokens&&(u.max_input_tokens=f);let p=parseFloat(o.value);!isNaN(p)&&p!==pe.compact_trigger_pct&&(u.compact_trigger_pct=p);let he=parseFloat(s.value);!isNaN(he)&&he!==pe.compact_retain_pct&&(u.compact_retain_pct=he),c.value!==(pe.summary_model||``)&&(u.summary_model=c.value),l.value!==(pe.summary_provider||``)&&(u.summary_provider=l.value),Object.keys(u).length>0&&(e.context=u);let _e={},D=parseInt(d.value,10);!isNaN(D)&&D!==me.max_messages&&(_e.max_messages=D);let be=parseInt(m.value,10);!isNaN(be)&&be!==me.max_context_tokens&&(_e.max_context_tokens=be);let xe=parseInt(h.value,10);!isNaN(xe)&&xe!==me.idle_timeout_secs&&(_e.idle_timeout_secs=xe),g.value!==me.auto_archive&&(_e.auto_archive=g.value);let Se=parseInt(_.value,10);!isNaN(Se)&&Se!==me.archive_ttl_secs&&(_e.archive_ttl_secs=Se),Object.keys(_e).length>0&&(e.session=_e);let Ce={};v.value&&v.value!==(ge.shell_policy||``)&&(Ce.shell_policy=v.value),y.value!==(ge.sandbox_root||``)&&(Ce.sandbox_root=y.value);let O=parseInt(b.value,10);!isNaN(O)&&O!==ge.timeout_secs&&(Ce.timeout_secs=O);let we=parseInt(x.value,10);!isNaN(we)&&we!==ge.max_output_bytes&&(Ce.max_output_bytes=we),Object.keys(Ce).length>0&&(e.tools=Ce);let k={},Te={},Ee=parseInt(te.value,10);te.value!==``&&!isNaN(Ee)&&Ee!==ve.thinking_budget_tokens&&(Te.thinking_budget_tokens=Ee),re.value&&ne.value!==!!ve.prompt_cache_enabled&&(Te.prompt_cache_enabled=ne.value),Object.keys(Te).length>0&&(k.anthropic=Te);let De={},A=ye.reasoning_effort||``;ie.value!==A&&(De.reasoning_effort=ie.value),Object.keys(De).length>0&&(k.openai=De);let j={},Oe=parseInt(ae.value,10);ae.value!==``&&!isNaN(Oe)&&Oe!==w.thinking_budget&&(j.thinking_budget=Oe),se.value&&oe.value!==!!w.cache_enabled&&(j.cache_enabled=oe.value);let ke=parseInt(ce.value,10);ce.value!==``&&!isNaN(ke)&&ke!==w.cache_ttl_seconds&&(j.cache_ttl_seconds=ke),Object.keys(j).length>0&&(k.gemini=j),Object.keys(k).length>0&&(e.llm=k),ee.value&&ee.value!==(C.model||``)&&(e.model=ee.value),S.value&&S.value!==(C.provider||``)&&(e.provider=S.value);let Ae=!1;if(Object.keys(e).length>0)try{let t=await cc(e);t&&t.restart_required&&(Ae=!0),await vc()}catch(e){let t=Array.isArray(e.errors)?e.errors.join(`; `):null;fe.value=t||e.message||`Failed to save server settings`}if(ue.value&&T.value){let e=E.value.find(e=>e.id===T.value),t=cm(e&&e.debug_mode,le.value);if(Object.keys(t).length>0)try{await Hp(T.value,t);let e=await Bp();e&&Array.isArray(e.agents)&&(E.value=e.agents)}catch(e){fe.value=e.error?.message||e.message||`Failed to save debug mode`}}fe.value||(n.value=!0,r.value=Ae),de.value=!1,!fe.value&&!Ae&&setTimeout(()=>t(),600)},be=e=>{e.target===e.currentTarget&&t()},xe=ge.enabled||C.enabled_tools||[];return f`
        <div class="settings-overlay open" onClick=${be}>
            <div class="settings-modal">
                <h2>Settings</h2>

                <!-- Security: API Keys -->
                <${mh} />

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
                <${fh} key="debug" title="Debug" defaultOpen=${!1}>
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
                                   checked=${le.value}
                                   disabled=${!T.value}
                                   onChange=${e=>{le.value=e.target.checked,ue.value=!0}} />
                            <span>${le.value?`enabled`:`disabled`}</span>
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
                <${fh} key="defaults" title="Default LLM (model / provider)" defaultOpen=${!0}>
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
                               placeholder=${C.model||`model id`}
                               value=${ee.value}
                               onInput=${e=>{ee.value=e.target.value}} />
                        <span class="settings-effective">
                            <${Qp} value=${ee.value.trim()} defaultValue=${C.model} />
                        </span>
                    <//>
                    <${$} label="Default LLM provider"
                        desc="Provider whose [llm.providers.NAME] entry the resolved model is sent to. Must be configured under [llm.providers] in alms.toml with a resolvable API key.">
                        <select class="settings-select settings-input-sm"
                                value=${S.value}
                                onChange=${e=>{S.value=e.target.value}}>
                            ${(C.llm_providers&&C.llm_providers.length>0?C.llm_providers:dh).map(e=>{let t=Xp(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                    <datalist id="model-suggestions">
                        ${Kp.map(e=>f`<option value=${e} key=${e}></option>`)}
                    </datalist>
                <//>

                <!-- Context (server-level, editable) -->
                <${fh} key="ctx" title="Context" defaultOpen=${!1}>
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
                <${fh} key="summary" title="Summary (compact strategy + episodic memory)" defaultOpen=${!1}>
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
                            <${Qp} value=${c.value.trim()} defaultValue=${C.model} />
                        </span>
                    <//>
                    <${$} label="Summary provider"
                        desc="Dedicated provider for the summary task. Must be configured under [llm.providers.<name>] with a resolvable API key. Set together with Summary model.">
                        <select class="settings-select settings-input-sm"
                                value=${l.value}
                                onChange=${e=>{l.value=e.target.value}}>
                            <option value="">Unset (no dedicated summary task)</option>
                            ${(C.llm_providers&&C.llm_providers.length>0?C.llm_providers:dh).map(e=>{let t=Xp(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                <//>

                <!-- Session (server-level, editable) -->
                <${fh} key="sess" title="Session" defaultOpen=${!1}>
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
                <${fh} key="tools" title="Tools" defaultOpen=${!1}>
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
                    <${ph} label="Enabled tools" value=${`${xe.length} tools`}
                        desc=${xe.join(`, `)} />
                <//>

                <!-- LLM Providers (server-level, editable) — #809 / #804 Slice A -->
                <${fh} key="llm" title="LLM Providers" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Server-level reasoning &amp; caching defaults. Mutations propagate to the next HTTP-triggered run without restart; Telegram-triggered runs use a boot-time snapshot until the daemon is restarted.
                    </span>

                    <h4 class="settings-llm-subhead">Anthropic</h4>
                    <${$} label="Thinking budget tokens"
                        desc="0 = extended thinking off. Leave blank to keep the current server value. The wire surface has no clear sentinel — once PATCHed, revert by editing settings.json + restart.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${ve.thinking_budget_tokens==null?`unset`:String(ve.thinking_budget_tokens)}
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
                                value=${ie.value}
                                onChange=${e=>{ie.value=e.target.value}}>
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
                               placeholder=${w.thinking_budget==null?`unset`:String(w.thinking_budget)}
                               value=${ae.value}
                               onInput=${e=>{ae.value=e.target.value}} />
                    <//>
                    <${$} label="Cache enabled"
                        desc="Gemini context caching via cachedContents. Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${oe.value}
                                   onChange=${e=>{oe.value=e.target.checked,se.value=!0}} />
                            <span>${oe.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                    <${$} label="Cache TTL (seconds)"
                        desc="Lifetime of a Gemini cache entry. Must be > 0.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="60"
                               placeholder=${w.cache_ttl_seconds==null?`300`:String(w.cache_ttl_seconds)}
                               value=${ce.value}
                               onInput=${e=>{ce.value=e.target.value}} />
                    <//>
                <//>

                <!-- Logging (server-level, read-only) -->
                <${fh} key="log" title="Logging" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        File-based logging settings. Requires restart to change.
                    </span>
                    <${ph} label="File logging" value=${he.file_enabled==null?`--`:he.file_enabled?`enabled`:`disabled`}
                        desc="Whether persistent file logging is active." />
                    <${ph} label="File level" value=${he.file_level||`--`}
                        desc="Log level for file output (trace, debug, info, warn, error)." />
                    <${ph} label="Rotation" value=${he.rotation||`--`}
                        desc="Log rotation policy: daily, hourly, or never." />
                    <${ph} label="Log directory" value=${he.log_dir||`default (data/logs/)`}
                        desc="Directory where log files are written." />
                <//>

                <div class="settings-divider"></div>

                <!-- Server info (compact) -->
                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Version: <span class="settings-info-value">${C.version||`unknown`}</span></div>
                        <div>Base URL: <span class="settings-info-value">${C.base_url||`unknown`}</span></div>
                        <div>Stream timeout: <span class="settings-info-value">${C.stream_chunk_timeout_secs||180}s</span></div>
                    </div>
                </div>

                ${fe.value&&f`
                    <div class="settings-error">
                        Failed to save server settings: ${fe.value}
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
                    <button class="settings-save" onClick=${D}
                            disabled=${de.value}>
                        ${de.value?`Saving...`:n.value?`Saved!`:`Apply`}
                    </button>
                </div>
            </div>
        </div>
    `}function gh(){let e=u(``),t=u(``),n=u(!1);return f`
        <div id="onboarding">
            <form class="onboard-card" onSubmit=${async r=>{r.preventDefault();let i=e.value.trim();if(i){if(!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(i)){t.value=`Invalid name: lowercase letters, digits, hyphens only (1-64 chars, no trailing hyphen)`;return}n.value=!0,t.value=``;try{let e=await Vp({name:i,is_default:!0});E.value=(await Bp()).agents||[];let t=e.id||(E.value.find(e=>e.name===i)||{}).id;t?await ld(t):console.warn(`[onboarding] POST /agents returned no id for agent:`,i,e)}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}}}}>
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
    `}function _h(e){if(!e)return``;if(e.status===`done`)return`Done`;if(e.status===`fail`)return`Failed`;if(e.status===`cancelled`)return`Cancelled`;let t=e.activity;if(!t||!t.kind)return`Starting…`;switch(t.kind){case`reasoning`:return`Reasoning…`;case`writing`:return`Writing…`;case`tool_start`:return t.tool?`Using ${t.tool}`:`Using tool`;case`tool_end`:return`Running…`;default:return`Running…`}}function vh(){let e=Object.entries(J.value);return e.length===0?null:f`
        <div class="sa-bar" aria-label="Subagent status bar">
            ${e.map(([e,t])=>{let n=t.status===`running`,r=t.status===`done`?`✓`:`✗`,i=t.displayName||e,a=_h(t),o=()=>{t.sessionId&&fl(t.sessionId)},s=e=>{sm(e)&&o()},c=e=>{om(e)&&(e.preventDefault(),o())},l=t.task?`${i}: ${t.task} — open subagent session`:`${i} — open subagent session`,u=Nc(t.sessionId),d=e=>t=>{if(t.stopPropagation(),t.key===`Escape`){t.preventDefault(),Fc();return}(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),e(t))},p=e=>{e.stopPropagation(),Pc(t.sessionId)},m=e=>{e.stopPropagation(),Lc(t.sessionId)},h=e=>{e.stopPropagation(),Fc()};return f`
                    <div class="sa-chip ${n?`running`:t.status}"
                         role="button"
                         tabindex="0"
                         title=${l}
                         onClick=${s}
                         onKeyDown=${c}>
                        ${n?f`<span class="tc-spinner"></span>`:f`<span>${r}</span>`}
                        <span class="sa-chip-name">${i}</span>
                        ${a&&f`<span class="sa-chip-status">${a}</span>`}
                        ${Mc(t)&&(u?f`
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
    `}function yh(){let e=D.value,{phase:t,detail:i}=gl.value,a=vl(t,i),o=u(!1),s=u(!1),c=n(null),l=r(()=>{o.value=!o.value},[]),d=r(()=>{o.value=!1,s.value=!0},[]);return p(()=>{if(!o.value)return;let e=e=>{c.current&&!c.current.contains(e.target)&&(o.value=!1)};return document.addEventListener(`click`,e,!0),()=>document.removeEventListener(`click`,e,!0)},[o.value]),e?f`
        <div class="agent-header-bar">
            <div class="agent-header-bar-left">
                <span class="agent-header-bar-name">${e.name}</span>
                ${a&&f`
                    <span class="agent-status-label">${a}</span>
                `}
            </div>
            <div class="agent-header-bar-right">
                <button class="hbtn agent-bar-btn ${ud.value===`workspace`?`active`:``}"
                        title="Workspace files"
                        aria-label="Open workspace panel"
                        onClick=${()=>dd(`workspace`)}>
                    <${_d} />
                    <span class="agent-bar-btn-label">Workspace</span>
                </button>
                <button class="hbtn agent-bar-btn ${ud.value===`timeline`?`active`:``}"
                        title="Agent timeline"
                        aria-label="Open timeline panel"
                        onClick=${()=>dd(`timeline`)}>
                    <${gd} />
                    <span class="agent-bar-btn-label">Timeline</span>
                </button>
                <button class="hbtn agent-bar-btn ${ud.value===`runs`?`active`:``}"
                        title="Agent runs"
                        aria-label="Open runs panel"
                        onClick=${()=>dd(`runs`)}>
                    <${xd} />
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
                <${hm}
                    agent=${e}
                    onClose=${()=>{s.value=!1}} />
            `}
        </div>
    `:null}var bh=d(!1),xh=new Set;function Sh(e,t,n){return e.fromAgent?e.fromAgent===t[0]?`left`:`right`:e.type===`agent`||e.role===`assistant`?n?n===t[0]?`left`:`right`:`left`:e.type===`user`||e.role===`user`?n?n===t[0]?`right`:`left`:`right`:`center`}function Ch({msg:e,participants:t,perspectiveAgent:r}){let i=Sh(e,t,r),a=e.fromAgent||(i===`left`?t[0]:t[1])||`?`,o=s(e.text||``),c=e.type===`agent`||e.role===`assistant`,l=n(null);return p(()=>{c&&gf(l.current)},[o,c]),f`
        <div class="dm-msg dm-msg-${i}">
            <div class="dm-msg-name-row dm-msg-name-row-${i}">
                <div class="dm-msg-name">${a}</div>
                <${_f} ts=${e.ts} />
            </div>
            <div class="dm-msg-bubble markdown-body" ref=${l}
                 dangerouslySetInnerHTML=${{__html:o}} />
        </div>
    `}function wh({text:e}){return f`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">${e}</span>
        </div>
    `}function Th(e,t){if(!e)return!1;let n=e.trim();if(!n)return!1;for(let e of t||[]){if(e.tool!==`send_message`)continue;let t=e.params&&typeof e.params.message==`string`?e.params.message.trim():``;if(t&&t===n)return!0}return!1}function Eh({runId:e,agentName:t,thinkingText:n,tools:r,status:i,isLive:a}){let[s,c]=o(!1),l=a&&Vl.value.get(e)||``,u=n||l,d=Th(u,r)?``:u,p=(r||[]).filter(e=>!(e.tool===`send_message`&&e.status===`done`)),m=p.length,h=(r||[]).length>0;return!a&&!h&&(!d||!d.trim())?null:f`
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
                        <${lp} key=${e.id} ...${e} />
                    `)}
                </div>
            `}
        </div>
    `}async function Dh(){let e=x.value;if(!(!e||bh.value)){bh.value=!0;try{await hc(e)}catch(e){console.error(`[cancel-dm] failed:`,e)}finally{bh.value=!1}}}function Oh(){let e=n(null),r=S.value;p(()=>{let n=0,r=t(()=>{A.value,cancelAnimationFrame(n),n=requestAnimationFrame(()=>{ef(e.current)})});return()=>{cancelAnimationFrame(n),r()}},[]);let i=A.value,a=D.value?D.value.name:null,o=r.length>=2?`${r[0]} <-> ${r[1]}`:`DM conversation`,s=!!O.value,c=!!_l.value,l=s||c,u=bh.value;return f`
        <div class="dm-view-header">
            <span class="dm-view-header-icon" aria-hidden="true">\u2194</span>
            <span class="dm-view-header-label">${o}</span>
            <span class="dm-view-header-badge">read-only</span>
        </div>
        <div class="dm-thread" ref=${e}>
            ${i.length===0&&f`
                <div class="empty-state">No messages in this conversation yet.</div>
            `}
            ${i.map(e=>{if(e.type===`dm_ended`){let t=`Conversation ended -- ${e.reason||`ended`}`;return f`<${wh} key=${e.id} text=${t} />`}if(e.type===`system`)return f`<${wh} key=${e.id} text=${e.text} />`;if(e.type===`notification`){let t=e.metadata||{};if(t.type===`dm_ended_notification`){let n=`DM with ${t.peer||`unknown`} ended -- ${wl[t.reason]||t.reason||`ended`}`;return f`<${wh} key=${e.id} text=${n} />`}return f`<${wh} key=${e.id} text=${e.text} />`}if(e.type===`error`)return f`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-error">${e.text}</div></div>`;if(e.type===`tokens`)return null;if(e.type===`thinking`){let t=`Thinking…`;if(e.pending)t=`Sending…`;else if(e.queuedBehind>0)t=`Queued \u2014 position ${e.queuedBehind}\u2026`;else if(e.source){let n=e.source.startsWith(`peer:`)?e.source.slice(5):e.source;n&&(t=`${n} is thinking\u2026`)}return f`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-thinking">${t}</div></div>`}if(e.type===`warning`)return f`<${wh} key=${e.id} text=${e.text||`Warning`} />`;if(e.type===`run_boundary`){if(!e.status||e.status===`completed`)return null;let t=e.status===`failed`?`run failed`:e.status===`cancelled`?`run cancelled`:`run ${e.status}`;return f`<${wh} key=${e.id} text=${t} />`}if(e.type===`subagent_completed`){let t=`Subagent '${e.name||`subagent`}' ${e.status===`fail`?`failed`:`completed`}`;return f`<${wh} key=${e.id} text=${t} />`}if(e.type===`job_completed`)return f`<${wh} key=${e.id} text=${`Job '${e.jobName||`job`}' ${e.status||`completed`}`} />`;if(e.type===`context_debug`)return f`<${hp} key=${e.id} ...${e} />`;if(e.type===`dm_reasoning`)return f`<${Eh} key=${e.id} ...${e} />`;if(e.type===`tool`){if(e.tool===`send_message`&&e.status===`done`&&!e.error)return null;xh.has(e.id)||(xh.add(e.id),console.warn(`[DmConversationView] ungrouped DM tool rendered as a standalone sibling row — this fallback is meant to be dead post-#1076/#1154. Tool:`,e.tool,`id:`,e.id,`runId:`,e.runId));let t=Sh({type:`agent`,role:`assistant`},r,a),n=t===`left`?r[0]:r[1];return f`
                        <div key=${e.id} class="dm-msg dm-msg-${t} dm-msg-tool-row">
                            <div class="dm-msg-name">${n||`?`}</div>
                            <${lp} ...${e} />
                        </div>
                    `}if(e.type===`image`){let t=Sh(e,r,a),n=e.fromAgent||(t===`left`?r[0]:r[1])||`?`;return f`
                        <div key=${e.id} class="dm-msg dm-msg-${t}">
                            <div class="dm-msg-name-row dm-msg-name-row-${t}">
                                <div class="dm-msg-name">${n}</div>
                                <${_f} ts=${e.ts} />
                            </div>
                            <div class="dm-msg-bubble">
                                ${e.url?f`<img src=${e.url} alt=${e.alt||``} class="dm-msg-image" />`:`[Image${e.alt?`: `+e.alt:``}]`}
                            </div>
                        </div>
                    `}return e.type===`user`||e.type===`agent`?f`<${Ch} key=${e.id} msg=${e} participants=${r} perspectiveAgent=${a} />`:null})}
        </div>
        <div class="dm-view-footer">
            ${l?f`
                    <button class="dm-cancel-btn"
                            disabled=${u}
                            title="Stop this DM conversation"
                            aria-label="Stop conversation"
                            onClick=${Dh}>
                        <span class="dm-cancel-btn-icon" aria-hidden="true">\u25A0</span>
                        ${u?`Stopping…`:`Stop conversation`}
                    </button>
                `:f`
                    <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
                `}
        </div>
    `}function kh(){return Tl.value?f`
        <button
            type="button"
            class="stream-dead-banner"
            role="alert"
            aria-live="polite"
            onClick=${Nl}
            title="Click to reconnect live updates"
        >
            <span class="stream-dead-banner-icon" aria-hidden="true">⚠</span>
            <span class="stream-dead-banner-text">
                Live updates disconnected — click to reconnect or reload.
            </span>
        </button>
    `:null}t(()=>{let e=D.value;document.title=e?`ALMS - ${e.name}`:`ALMS`});var Ah=d(`connecting...`);function jh(e){let t=[],n=0;for(;n<e.length;)if(e[n].type===`tool`){let r=[];for(;n<e.length&&e[n].type===`tool`;)r.push(e[n]),n++;r.length>1?t.push({_isToolGroup:!0,key:`tg-`+r[0].id,tools:r}):t.push(r[0])}else t.push(e[n]),n++;return t}function Mh(){let e=n(null);p(()=>{let n=0,r=t(()=>{A.value,cancelAnimationFrame(n),n=requestAnimationFrame(()=>{ef(e.current)})});return()=>{cancelAnimationFrame(n),r()}},[]);let r=jh(A.value),i=y.value,a=h.value,o=ve.value,s=ie.value,c=_e.value,l=o?s?.agent_name?s.agent_name+` notifications`:`Notification session`:s?.session_type===`job`?c?c+` job session`:`Job session`:s?.session_type===`subagent`?`Subagent session`:`Internal session`,u=o?`⚡`:s?.session_type===`job`?`⏰`:`⚙`,d=s?.session_type?`internal-session-`+s.session_type:``;return f`
        <div id="chat">
            <${yh} />
            ${(Cc.value||Sc.value)&&f`
                <div id="messages" role="log" aria-live="polite">
                    ${Cc.value?f`<div class="loading-state">Loading agent...</div>`:f`<div class="loading-state">Loading session...</div>`}
                </div>
            `}
            ${!Cc.value&&!Sc.value&&i&&f`
                <${Oh} />
            `}
            ${!Cc.value&&!Sc.value&&!i&&f`
            ${a&&f`
                <div class="internal-session-header ${d}">
                    <span class="internal-session-header-icon" aria-hidden="true">${u}</span>
                    <span class="internal-session-header-label">${l}</span>
                    <span class="internal-session-header-badge">read-only</span>
                </div>
            `}
            ${qc.value&&f`
                <div class="sa-breadcrumb">
                    <button class="sa-breadcrumb-btn" onClick=${()=>pl()}>
                        \u2190 Back to parent session
                    </button>
                    ${O.value&&(Nc(x.value)?f`
                            <span class="sa-cancel-confirm-group sa-breadcrumb-cancel" role="group"
                                  aria-label="Confirm cancel subagent"
                                  onKeyDown=${e=>{e.key===`Escape`&&(e.preventDefault(),Fc())}}>
                                <span class="sa-cancel-confirm-label">Cancel this subagent?</span>
                                <button class="sa-confirm-btn sa-confirm-yes"
                                        title="Yes, cancel this subagent"
                                        onClick=${()=>Lc(x.value)}>Yes</button>
                                <button class="sa-confirm-btn sa-confirm-no"
                                        title="No, keep it running"
                                        onClick=${()=>Fc()}>No</button>
                            </span>
                        `:f`
                            <button class="sa-breadcrumb-cancel-btn sa-breadcrumb-cancel"
                                    title="Cancel this subagent"
                                    onClick=${()=>Pc(x.value)}>
                                Cancel subagent
                            </button>
                        `)}
                </div>
            `}
            <div id="messages" role="log" aria-live="polite" ref=${e}>
                ${A.value.length===0&&f`
                    <div class="empty-state">
                        ${a?`No activity recorded in this session yet.`:`No messages yet. Send a message to start.`}
                    </div>
                `}
                ${r.map(e=>{if(e._isToolGroup)return f`
                            <${up} key=${e.key} count=${e.tools.length}>
                                ${e.tools.map(e=>f`<${lp} key=${e.id} ...${e} />`)}
                            <//>
                        `;let t=e;if(t.type===`user`||t.type===`agent`)return f`<${bf} key=${t.id} type=${t.type} text=${t.text} sealed=${t.sealed} fromAgent=${t.fromAgent} reasoning=${t.reasoning} ts=${t.ts} />`;if(t.type===`tool`)return f`<${lp} key=${t.id} ...${t} />`;if(t.type===`context_debug`)return f`<${hp} key=${t.id} ...${t} />`;if(t.type===`approval`)return f`<${_p} key=${t.id} ...${t} />`;if(t.type===`job_completed`)return f`<${Ep} key=${t.id} jobName=${t.jobName} status=${t.status} summary=${t.summary} ts=${t.ts} runId=${t.runId} truncated=${t.truncated} jobSessionUuid=${t.jobSessionUuid} jobSessionId=${t.jobSessionId} />`;if(t.type===`subagent_completed`)return f`<${jp} key=${t.id}
                            name=${t.name} task=${t.task} status=${t.status}
                            toolCount=${t.toolCount} durationMs=${t.durationMs}
                            sessionId=${t.sessionId} summary=${t.summary} />`;if(t.type===`image`){let e=!!t.fromAgent,n=t.role===`user`&&!e?`user`:`agent`,r=_e.value||D.value?.name,i=t.role===`user`&&!e?`>`:t.fromAgent?`${t.fromAgent} $`:r?`${r} $`:`$`;return f`
                            <div key=${t.id} class="msg ${n}">
                                <div class="msg-label-row">
                                    <div class="msg-label">${i}</div>
                                    ${t.ts&&f`<${_f} ts=${t.ts} />`}
                                </div>
                                <div class="msg-body">
                                    ${t.url?f`<img src=${t.url} alt=${t.alt||``} style="max-width:100%;border-radius:8px;" />`:`[Image${t.alt?`: `+t.alt:``}]`}
                                    ${t.alt&&f`<div style="font-size:var(--text-xs);color:var(--text-secondary);margin-top:var(--space-2);">${t.alt}</div>`}
                                </div>
                            </div>
                        `}if(t.type===`error`)return f`<${Sf} key=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`warning`)return f`<${Cf} key=${t.id} id=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`run_boundary`)return f`<${Tf} key=${t.id} status=${t.status} error=${t.error} />`;if(t.type===`system`)return f`<${wf} key=${t.id} text=${t.text} />`;if(t.type===`dm_ended`)return f`<${Ef} key=${t.id} peer=${t.peer} reason=${t.reason} />`;if(t.type===`notification`){let e=t.metadata||{};return e.type===`dm_ended_notification`?f`<${Ef} key=${t.id} peer=${e.peer||`unknown`} reason=${wl[e.reason]||e.reason||`conversation ended`} />`:f`<${wf} key=${t.id} text=${t.text} />`}if(t.type===`tokens`)return f`<${xf} key=${t.id} usage=${t.usage} />`;if(t.type===`thinking`){let e=`Thinking`,n=`thinking-indicator`;t.pending?(e=`Sending`,n=`pending-indicator`):t.queuedBehind>0?(e=`Queued \u2014 position ${t.queuedBehind}`,n=`queued-indicator`):t.source&&t.source.startsWith(`peer:`)?e=`Replying to message from `+t.source.slice(5):t.source===`job`?e=`Running scheduled job`:t.source===`subagent`&&(e=`Processing subagent result`);let r=D.value?.name||`Agent`;return f`
                            <div key=${t.id} class="msg agent">
                                <div class="msg-label">${r} $</div>
                                <div class="msg-body ${n}">${e}</div>
                            </div>
                        `}return null})}
            </div>
            <${Np} />
            <${vh} />
            ${a?f`
                    <div class="internal-session-footer">
                        <span class="internal-session-footer-text">This is a read-only view of internal agent activity.</span>
                    </div>
                `:f`<${zp} />`}
            `}
        </div>
    `}function Nh(){let e=u(!1);return f`
        <${jd} status=${Ah} onOpenSettings=${()=>{e.value=!0}} />
        <${kh} />
        ${E.value.length>0?f`
                <div id="main">
                    <${Xd} />
                    <${Mh} />
                    <${sh} />
                </div>`:f`<${gh} />`}
        <${hh} open=${e.value} onClose=${()=>{e.value=!1}} />
    `}i(f`<${Nh} />`,document.getElementById(`app`));function Ph(){wc.value=!1,Ah.value=`connecting...`,od().then(()=>{Ah.value=`connected`}).catch(()=>{Ah.value=`offline`,wc.value=!0})}Ec(Ph),Fl(),Ph();