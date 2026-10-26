import{t as e}from"./rolldown-runtime-DK3Fl9T5.js";import{a as t,c as n,d as r,i,l as a,n as o,o as s,r as c,s as l,u}from"./index-7VlRUHHy.js";import{n as d,t as f}from"./deps-B_HGIh8D.js";import{A as p,C as m,D as h,E as g,M as _,N as v,O as y,P as b,S as x,T as S,_ as C,a as w,b as ee,c as T,d as E,f as te,g as ne,h as re,i as ie,j as D,k as O,l as k,m as A,n as j,o as M,p as N,r as ae,s as P,t as oe,u as se,v as ce,w as le,x as F,y as I}from"./pending-messages-Kp_HgQ76.js";import{A as ue,C as L,D as de,E as fe,F as pe,I as R,M as me,N as he,O as ge,P as _e,S as ve,T as ye,_ as be,a as xe,b as Se,c as Ce,d as we,f as Te,g as Ee,h as De,i as Oe,j as ke,k as Ae,l as je,m as Me,n as Ne,o as Pe,p as Fe,r as Ie,s as Le,t as Re,u as ze,v as Be,w as Ve,x as He,y as Ue}from"./use-session-stream-DJBy8j31.js";import{t as z}from"./entity-state-Dvifu_4n.js";import{a as We,n as B,r as V,t as H}from"./agents-jsJzSRYf.js";import{activeRunId as U,clearRuns as Ge,replaceRuns as Ke,runListGeneration as qe,runs as Je,selectedRunId as Ye,setRunStatus as Xe,upsertRun as Ze}from"./runs-CUTlVxg0.js";import{a as Qe,i as $e,o as et,r as tt,s as nt,t as rt}from"./chat-actions-uXEv39RD.js";import{n as it,t as at}from"./select-generation-DvILpFQd.js";import{a as ot,c as st,i as ct,n as lt,o as ut,r as dt,s as ft,t as pt}from"./runs-CXFz47fC.js";var mt=()=>D(`/settings`),ht=e=>_(`/settings`,e),W=s({});async function gt(){try{W.value=await mt()}catch(e){console.error(`[settings] refresh failed:`,e)}}var G=s(null),K=s(null),_t=s(!1),vt=s(!1),yt=s(!1),bt=null;function xt(e){bt=e}function St(){bt&&bt()}var q=null,Ct=null,wt=0,Tt=10,Et=null,J=null,Dt=null;function Ot(e,t){let n=t&&t.streamEpoch!=null?String(t.streamEpoch):null;if(At(),!e)return;Dt!==null&&(clearTimeout(Dt),Dt=null);let r=localStorage.getItem(`alms_auth_token`),i=new URLSearchParams;r&&i.set(`token`,r),t&&t.lastEventId!=null&&i.set(`last_event_id`,String(t.lastEventId)),n&&i.set(`stream_epoch`,n);let a=i.toString(),o=`/events/session-activity${a?`?`+a:``}`,s=new EventSource(o);q=s,Ct=e,wt=0,Et=t&&t.lastEventId!=null?t.lastEventId:null,J=n;let c=!1,l=!1,u=!1,d=null,f=null,p=0,m=!1,h=(e,t,n)=>k(e,t,n,J),g=async(e,t=J)=>{let n=Number.isSafeInteger(e)?e:null,r=t;if(l){u=!0,f===r?n!=null&&(d=d==null?n:Math.max(d,n)):(f=r,d=n);return}if(s!==q)return;l=!0;let i=se(n,r),a=null;try{a=await R(null,{includeDms:!0})}catch(e){console.error(`[agent-events] activity reconciliation failed:`,e)}if(s!==q){T(i);return}a?te(i,a.sessions||[])?p=0:(u=!0,d=n,f=r):T(i),l=!1;let o=u,c=d,m=f;if(u=!1,d=null,f=null,o&&s===q)g(c,m);else if(!a&&s===q){p++;let e=Math.min(1e3*2**(p-1),3e4);s._reconciliationRetryTimer=setTimeout(()=>{s._reconciliationRetryTimer=null,s===q&&g(null)},e)}};s.addEventListener(`open`,()=>{if(s!==q)return;let e=c||!!(t&&t.reconcileOnOpen);c=!0,xe(`agent-events`),e&&g(null)});let _=async(t,n)=>{if(!(m||s!==q)){m=!0,s.close(),console.error(`[agent-events] rejected live event; reconciling:`,n);try{let n=await R(null,{includeDms:!0});if(s!==q)return;A(n.sessions||[],t,J),q=null,Ot(e,{lastEventId:t,streamEpoch:J,reconcileOnOpen:!0})}catch(e){console.error(`[agent-events] contract reconciliation failed:`,e),Le(`agent-events`)}}},v=(e,t)=>s.addEventListener(e,n=>{if(s!==q)return;let r=n.lastEventId,i=r&&/^\d+$/.test(r)?Number(r):null;try{let a=globalThis.__almsContracts;if(!a)throw Error(`Frontend contract bridge is not installed`);t({data:a.parseSseJsonPayload(e,n.data),lastEventId:n.lastEventId}),i!=null&&(Et=r)}catch(t){i==null?(console.error(`[agent-events]`,e,`handler failed:`,t),s.close(),Le(`agent-events`)):_(i,t)}});v(`session_activity_started`,e=>{let t=e.data,n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;h(`session_activity_started`,t,n)}),v(`session_activity_ended`,e=>{let t=e.data,n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;h(`session_activity_ended`,t,n)}),v(`stream_state`,e=>{let t=e.data,n=!!(J&&t.stream_epoch&&J!==t.stream_epoch);if(t.stream_epoch&&(J=t.stream_epoch),t.requires_reconciliation||n){let e=Number.isSafeInteger(t.newest)?t.newest:null;g(e)}}),s.onerror=()=>{if(s.readyState===EventSource.CLOSED){if(wt++,wt>=Tt){console.error(`[agent-events] Max retries reached for agent`,e),Le(`agent-events`);return}let t=Math.min(2e3*2**(wt-1),3e4),n=e,r=Et,i=J;Dt=setTimeout(()=>{Dt=null,Ct===n&&Ot(n,{lastEventId:r,streamEpoch:i,reconcileOnOpen:!0})},t)}}}function kt(){wt=0;let e=Ct;e?Ot(e,{lastEventId:Et,streamEpoch:J,reconcileOnOpen:!0}):xe(`agent-events`)}je(kt);function At(){q&&=(q._reconciliationRetryTimer!=null&&(clearTimeout(q._reconciliationRetryTimer),q._reconciliationRetryTimer=null),q.close(),null),Ct=null,Et=null,J=null,wt=0,Dt!==null&&(clearTimeout(Dt),Dt=null),xe(`agent-events`)}function jt(e,t){let n=e||``,r=t&&t.job_status,i=n.indexOf(`
`),a=i>=0?n.slice(0,i):n,o=i>=0?n.slice(i+1).trim():``,s=a.match(/^\[Scheduled job (\w+)\]\s*(.*)$/);if(!s)return{jobName:n,status:r||`success`,summary:``};let c=s[1];return{jobName:(s[2]||``).trim(),status:r||(c===`failed`?`error`:c===`completed`?`success`:c===`finished`?`cancelled`:`success`),summary:o}}function Mt(e){let t=new Map;for(let n of e){let e=n.tool_id;if(!e)continue;let r=t.get(e)||{call:null,result:null,runId:null,invocationId:null};n.role===`assistant`||n.role===`Assistant`?(r.call=n,n.run_id&&(r.runId=n.run_id)):(n.role===`tool`||n.role===`Tool`)&&(r.result=n,n.run_id&&!r.runId&&(r.runId=n.run_id)),n.tool_invocation_id&&!r.invocationId&&(r.invocationId=n.tool_invocation_id),t.set(e,r)}return t}function Nt(e,t){let n=t&&t.hasActiveRun,r=t&&t.isDm,i=t&&t.sessionToolCalls||[],a=t&&t.terminalRunIds||null,o=e=>e&&a&&a.has(e)?`done`:n?`running`:`done`,s=i.length>0?Mt(i):new Map,c=new Map,l=new Map;for(let t of e)t.type===`tool_result`&&t.tool_id&&c.set(t.tool_id,t),t.type===`tool_result`&&t.metadata&&t.metadata.tool_invocation_id&&l.set(t.metadata.tool_invocation_id,t);let u=[],d=[],f=(e,t)=>{u.push(e),d.push(t||null)};for(let t of e)if(t.type===`text`||!t.type){if(t.metadata&&t.metadata.message_type===`dm_ended`){let e=t.metadata.reason||``;f({id:P(),type:`dm_ended`,peer:t.metadata.ended_by||`unknown`,reason:we[e]||e||`conversation ended`},t.timestamp);continue}if(t.metadata&&t.metadata.message_type===`reasoning`){let e=``;Array.isArray(t.metadata.reasoning_blocks)&&(e=t.metadata.reasoning_blocks.map(e=>e&&typeof e.text==`string`?e.text:``).join(``)),e||=t.content||``,f({id:P(),type:`dm_reasoning_text`,text:e,fromAgent:t.metadata.from_agent||null,runId:t.metadata.run_id||null},t.timestamp);continue}let e=t.role===`system`&&t.metadata&&t.metadata.synthetic,n=t.role===`user`&&t.metadata&&t.metadata.message_type===`dm`&&t.metadata.from_agent;if(e&&t.metadata.type===`job_notification`){let e=jt(t.content||``,t.metadata);f({id:P(),type:`job_completed`,jobName:e.jobName,status:e.status,summary:e.summary,ts:t.timestamp||null,metadata:t.metadata||null,runId:t.metadata&&t.metadata.run_id||null,truncated:t.metadata?t.metadata.truncated:void 0,jobSessionUuid:t.metadata&&t.metadata.job_session_uuid||null,jobSessionId:t.metadata&&t.metadata.job_session_id||null},t.timestamp);continue}if(e&&t.metadata.type===`run_boundary`){let e=t.metadata.status||`completed`;f({id:P(),type:`run_boundary`,status:e,runId:t.metadata.run_id||null,error:t.metadata.error||null,text:t.content||``},t.timestamp);continue}if(e&&t.metadata.kind===`error`){f({id:P(),type:`error`,text:t.metadata.error?`${t.content}\n\n${t.metadata.error}`.trim():t.content||`Run error`,code:t.metadata.error_kind||t.metadata.type||null},t.timestamp);continue}if(e&&t.metadata.type===`run_warning`){if(t.metadata.source_agent)continue;f({id:P(),type:`warning`,code:t.metadata.code||`UNKNOWN`,text:t.content||`Warning`},t.timestamp);continue}if(e&&t.metadata.type===`subagent_completion`){let e=t.metadata;f({id:P(),type:`subagent_completed`,name:e.subagent_name||`subagent`,task:e.task_description||``,status:e.status||`done`,toolCount:e.tool_count||0,durationMs:e.duration_ms==null?null:e.duration_ms,sessionId:e.session_id||null,summary:e.summary||``,toolInvocationId:e.tool_invocation_id||null},t.timestamp);continue}if(e&&t.metadata.type===`subagent_started`){let e=t.metadata;f({id:P(),type:`subagent_started`,name:e.subagent_name||`subagent`,toolInvocationId:e.tool_invocation_id||null,subagentSessionId:e.subagent_session_id||null},t.timestamp);continue}if(e&&t.metadata.type===`dm_ended_notification`){f({id:P(),type:`notification`,role:`system`,text:t.content||``,metadata:t.metadata,sealed:!0},t.timestamp);continue}let r=e?`notification`:n?`agent`:t.role===`user`?`user`:`agent`,i;r===`agent`&&t.metadata&&Array.isArray(t.metadata.reasoning_blocks)&&(i=t.metadata.reasoning_blocks.map(e=>e&&typeof e.text==`string`?e.text:``).join(``),i||=void 0),f({id:P(),type:r,role:t.role,text:t.content||``,metadata:t.metadata||null,sealed:!0,reasoning:i,fromAgent:n?t.metadata.from_agent:void 0,ts:t.timestamp||null},t.timestamp)}else if(t.type===`tool_call`){let e=t.metadata&&t.metadata.tool_call_id||null,n=t.metadata&&t.metadata.tool_invocation_id||null,r=(e?c.get(e):null)||(n?l.get(n):null),i=t.metadata&&t.metadata.run_id||null,a=t.metadata&&t.metadata.message_type===`reasoning`,u=t.tool,d=t.params,p=r?r.result:null,m=r?r.ok:null,h=i;if(e&&s.has(e)){let t=s.get(e);if(t.call&&(u||=t.call.tool_name||u,!d&&t.call.params))try{d=JSON.parse(t.call.params)}catch{d=null}if(t.result&&p==null){try{p=JSON.parse(t.result.result)}catch{p=t.result.result}m??=!0}!h&&t.runId&&(h=t.runId)}let g=a&&t.metadata&&t.metadata.from_agent?t.metadata.from_agent:void 0;if(!g&&e&&s.has(e)){let t=s.get(e);g=t.call&&t.call.from_agent||t.result&&t.result.from_agent||void 0}f({id:n||e||P(),type:`tool`,tool:u,params:d,status:m==null?o(h):m?`done`:`fail`,result:p,runId:h||void 0,isReasoning:a||void 0,fromAgent:g,ts:t.timestamp||null},t.timestamp)}else if(t.type===`image`){let e=t.role===`user`&&t.metadata&&t.metadata.message_type===`dm`&&t.metadata.from_agent;f({id:P(),type:`image`,role:e?`assistant`:t.role,url:t.url||``,alt:t.alt||``,sealed:!0,fromAgent:e?t.metadata.from_agent:void 0,ts:t.timestamp||null},t.timestamp)}if(i.length>0){let t=new Set;for(let e of u)e.type===`tool`&&e.id&&t.add(e.id);for(let n of e)if(n.type===`tool_call`){let e=n.metadata&&n.metadata.tool_call_id;e&&t.add(e);let r=n.metadata&&n.metadata.tool_invocation_id;r&&t.add(r)}let n=[];for(let[e,i]of s){if(t.has(e)||i.invocationId&&t.has(i.invocationId)||!i.call)continue;let a=null;if(i.call.params)try{a=JSON.parse(i.call.params)}catch{}let s=null,c=null;if(i.result&&i.result.result){try{s=JSON.parse(i.result.result)}catch{s=i.result.result}c=!0}let l=i.call.from_agent||i.result&&i.result.from_agent||void 0;n.push({entry:{id:i.invocationId||e||P(),type:`tool`,tool:i.call.tool_name||`unknown`,params:a,status:c==null?o(i.runId):c?`done`:`fail`,result:s,runId:i.runId||void 0,isReasoning:r||void 0,fromAgent:l,ts:i.call.timestamp||null},ts:i.call.timestamp||null})}if(n.length>0){n.sort((e,t)=>!e.ts&&!t.ts?0:e.ts?t.ts?e.ts<t.ts?-1:+(e.ts>t.ts):-1:1);let e=0;for(let{entry:t,ts:r}of n){if(!r){u.push(t),d.push(null);continue}let n=u.length;for(let t=e;t<d.length;t++)if(d[t]&&d[t]>r){n=t;break}u.splice(n,0,t),d.splice(n,0,r),e=n+1}}}return u}function Pt(e){let t=new Map,n=new Set,r=null;for(let i=0;i<e.length;i++){let a=e[i];if(a.type===`dm_reasoning_text`&&a.runId){n.add(i);let e=t.get(a.runId)||{agentName:null,thinkingText:``,tools:[],firstIdx:i};e.agentName=e.agentName||a.fromAgent,e.thinkingText=(e.thinkingText||``)+(a.text||``),t.has(a.runId)||t.set(a.runId,e),r=a.runId}else if(a.type===`tool`&&a.runId){n.add(i);let e=t.get(a.runId)||{agentName:null,thinkingText:``,tools:[],firstIdx:i};e.tools.push(a),e.agentName=e.agentName||a.fromAgent,t.has(a.runId)||t.set(a.runId,e),r=a.runId}else if(a.type===`tool`&&!a.runId&&r&&t.has(r)){n.add(i);let e=t.get(r);e.tools.push(a),e.agentName=e.agentName||a.fromAgent}else a.type!==`tool`&&(r=null)}if(t.size===0)return e;let i=[],a=new Set;for(let r=0;r<e.length;r++)if(n.has(r)){let n=e[r].runId;if(n&&t.has(n)&&!a.has(n)){let e=t.get(n);(e.tools.length>0||e.thinkingText&&e.thinkingText.trim())&&i.push({id:P(),type:`dm_reasoning`,runId:n,agentName:e.agentName,thinkingText:e.thinkingText||``,tools:e.tools,status:`done`,isLive:!1}),a.add(n)}}else i.push(e[r]);return i}function Ft(e,t){return typeof t==`number`&&Number.isFinite(t)&&e!=null&&Number.isFinite(Number(e))&&Number(e)>=t}function It(e){return e==null?``:String(e).replace(/\s+/g,`-`).replace(/[^A-Za-z0-9-]/g,``).replace(/-+/g,`-`).replace(/^-+|-+$/g,``)}var Lt=[`default`,`dm`,`workspace`],Rt=/^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32})$/i,zt=64;function Bt(e){return typeof e!=`string`||e.length===0?null:e.length>zt?{code:`AGENT_NAME_TOO_LONG`,message:`Agent name is too long (max ${zt} characters after normalization)`}:Lt.includes(e.toLowerCase())?{code:`AGENT_NAME_RESERVED`,message:`'${e}' is a reserved name`}:Rt.test(e)?{code:`AGENT_NAME_LOOKS_LIKE_UUID`,message:`'${e}' looks like a UUID (conflicts with ID-based lookup)`}:null}function Vt(e,t){return typeof e!=`string`||typeof t!=`string`?!1:e.toLowerCase()===t.toLowerCase()}function Ht(e,t){if(typeof e!=`string`||typeof t!=`string`)return null;let n=e.split(`:`);if(n.length<3||n[0]!==`dm`)return null;let[,r,i]=n;return Vt(r,t)?i||null:Vt(i,t)&&r||null}function Ut(e,t){return Array.isArray(e)?typeof t!=`string`||t===``?e[0]||null:e.some(e=>Vt(e,t))&&e.find(e=>!Vt(e,t))||null:null}var Wt=new Set([`subagent`,`job`,`episodic`,`notification`]),Gt=200,Kt=new Set([`completed`,`failed`,`cancelled`]),qt=100;function Jt(e,t){return t||y.value.find(t=>t.id===e)||I.value.find(t=>t.id===e)||null}async function Yt(e,t){let n=t.isStale,r=t.logPrefix||`loadSession`,i=null;try{let t=await he(e);if(n())return;i=t||null,t&&Wt.has(t.session_type)&&!y.value.some(e=>e.id===t.id)&&O(t,`pinned`),t&&t.session_type===`dm`&&!I.value.some(e=>e.id===t.id)&&O(t,`cross`),t&&Object.prototype.hasOwnProperty.call(t,`parent_session_id`)?L.value=t.parent_session_id??null:L.value=null}catch(e){if(n())return;if(console.warn(`[${r}] Failed to fetch session metadata:`,e),t.requireAuthoritativeSnapshot)throw e}let a=new Set;try{let t=await st(e,Gt);if(n())return;let r=t.runs||[];Ke(e,r);for(let e of r)e&&e.run_id&&Kt.has(e.status)&&a.add(e.run_id)}catch(r){if(n())return;if(t.requireAuthoritativeSnapshot)throw r;Ke(e,[])}let o=t.minimumLastEventId??null,s=!1,c=new Set;try{let[c,l]=await Promise.all([_e(e),pe(e).catch(e=>{if(console.warn(`[${r}] Failed to load session tool calls:`,e),t.requireAuthoritativeSnapshot)throw e;return{tool_calls:[]}})]);if(n())return;let u=c.messages||[],d=l.tool_calls||[];s=Jt(e,i)?.session_type===`dm`;let f=Nt(u,{hasActiveRun:!!U.value,sessionToolCalls:d,isDm:s,terminalRunIds:a}),p=u.filter(e=>e.type===`tool_call`).length,m=f.filter(e=>e.type===`tool`).length;(p>0||m>0||d.length>0)&&console.debug(`[${r}] history loaded:`,u.length,`API messages,`,p,`tool_calls ->`,m,`tool rows,`,d.length,`session tool call records`);let h=ae(e);for(let t of h)t.runId&&Je.value.some(e=>e.run_id===t.runId)?j(e,{messageId:t.messageId}):console.debug(`[loadSession] preserving pending user message for session`,e,t.messageId);let g=s?Pt(f):f;Qe(g,e),Ve(f);let _=c.last_event_id??null;_!=null&&(o==null||_>o)&&(o=_)}catch(r){if(n())return;if(Qe([{id:P(),type:`error`,text:`Failed to load message history: ${r.error?.message||r.message||`unknown error`}`}],e),t.requireAuthoritativeSnapshot)throw r}if(U.value){if(!M.value.some(e=>e.type===`thinking`)){let e=Je.value.find(e=>e.run_id===U.value),i=e&&e.status===`queued`,a=+!!i;if(i)try{let e=await dt(U.value);if(n())return;typeof e?.queue_position==`number`&&e.queue_position>0&&(a=e.queue_position)}catch(e){if(console.warn(`[${r}] Failed to load queue position:`,e),t.requireAuthoritativeSnapshot)throw e}let o=e?.agent_id||null,s=o&&V.value.find(e=>e.id===o)?.name||null;rt({id:P(),type:`thinking`,queuedBehind:a,runId:U.value,agentName:s})}try{let t=await ft(e);if(n())return;let r=t.approvals||[];if(r.length>0){let e=r.map(e=>{let t=Te(e);return{id:P(),type:`approval`,approvalId:t.approvalId,tool:t.tool,params:t.params,runId:t.runId,resolved:!1}});rt(...e)}}catch(e){if(console.warn(`[${r}] Failed to load pending approvals:`,e),t.requireAuthoritativeSnapshot)throw e}{let i=U.value,a=Je.value.find(e=>e.run_id===i),l=a&&(a.status===`running`||a.status===`queued`);try{let t=await ct(i);if(n())return;if(t?.terminal===!0){Ft(o,t.seal_event_id)&&c.add(i);try{let e=await dt(i);if(n())return;Ze(e)}catch(t){console.warn(`[loadSession] terminal run status refresh failed:`,t),Xe(i,`completed`,{sessionId:e})}$e(e=>e.type!==`thinking`||e.runId!==i)}s||(l&&t?.text&&rt({id:P(),type:`agent`,role:`assistant`,text:``,reasoning:t.text,sealed:!1,ts:new Date().toISOString()}),t?.last_event_id!=null&&(o==null||t.last_event_id>o)&&(o=t.last_event_id))}catch(e){if(console.warn(`[${r}] Failed to load in-flight reasoning:`,e),t.requireAuthoritativeSnapshot)throw e}if(!s)try{let e=await ot(i);if(n())return;l&&e?.text&&(nt(e=>e.type===`agent`&&!e.sealed,t=>({...t,text:(t.text||``)+e.text}))||rt({id:P(),type:`agent`,role:`assistant`,text:e.text,reasoning:``,sealed:!1,ts:new Date().toISOString()})),e?.last_event_id!=null&&(o==null||e.last_event_id>o)&&(o=e.last_event_id)}catch(e){if(console.warn(`[${r}] Failed to load in-flight text:`,e),t.requireAuthoritativeSnapshot)throw e}}}if(!n()){if(Ie(e,{lastEventId:o,streamEpoch:t.streamEpoch??null,sealedReasoningRunIds:c}),U.value){let t=Je.value.find(e=>e.run_id===U.value);if(!(t&&t.status===`queued`)){let t=Jt(e,i);if(t&&t.session_type===`dm`&&Array.isArray(t.participants)){let e=H.value?.name,n=Ut(t.participants,e);n?Be(n):be(`calling_llm`,null)}else be(`calling_llm`,null)}}else{let t=H.value?.id;t?Xt(t,e,n,r).catch(e=>console.warn(`[${r}] restoreGlobalAgentPhase uncaught:`,e)):Me()}}}async function Xt(e,t,n,r){try{let t=await ut(e,qt);if(n())return;let i=t.runs||[],a=i.find(e=>e.session_type===`dm`&&e.status===`running`);if(a&&a.context_id){let e=H.value?.name,t=Ht(a.context_id,e);if(t){Be(t),console.debug(`[${r}] restored cross-session DM status: Chatting with ${t}`);return}}i.find(e=>e.status===`running`)?be(`calling_llm`,null):Me()}catch(e){console.warn(`[${r}] Failed to check agent global status:`,e),Me()}}Oe(async(e,t,n)=>{await Yt(e,{isStale:()=>C.value!==e,logPrefix:`contract-reconcile`,minimumLastEventId:t,streamEpoch:n,requireAuthoritativeSnapshot:!0})});function Zt(e,t){return!t||typeof t!=`string`?e??null:t===e?null:t}function Qt(e){return!e||typeof e!=`string`?null:e}function $t(e,t){return!t||typeof t!=`string`||!e||typeof e!=`string`?!1:e===t}function en(e){let t=new Map;if(!Array.isArray(e))return t;for(let n of e){if(!n||typeof n.agent_id!=`string`||!n.agent_id)continue;let e=t.get(n.agent_id);e?e.push(n):t.set(n.agent_id,[n])}return t}function tn(e,t){if(!e||!t||typeof t!=`string`)return!1;if(e.session_type===`notification`)return Vt(e.agent_name,t);if(e.session_type===`dm`){let n=e.participants;return Array.isArray(n)&&n.some(e=>Vt(e,t))}return!1}function nn(e,t){if(!Array.isArray(e))return[];let n=e.map((e,n)=>({s:e,idx:n,owned:+!tn(e,t)}));return n.sort((e,t)=>e.owned-t.owned||e.idx-t.idx),n.map(e=>e.s)}function rn(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type!==`notification`&&e.session_type!==`job`&&e.session_type!==`subagent`):[]}function an(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type===`job`):[]}function on(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type===`subagent`):[]}function sn(e){return!!e&&e.session_type!==`subagent`}var cn=`alms_active_agent`,Y=0;function ln(e){return`alms_active_session_${e}`}function un(e,t){e&&t&&localStorage.setItem(ln(e),t)}function dn(e,t,n){if(n){let e=t.find(e=>e.id===n);if(e)return e}let r=localStorage.getItem(ln(e));if(r){let e=t.find(e=>e.id===r);if(e)return e}return t[0]||null}async function fn(e,t){let n=localStorage.getItem(ln(e));if(!n||t.some(e=>e.id===n))return null;try{return await _e(n),n}catch(t){return t&&t.status===404&&localStorage.removeItem(ln(e)),null}}async function pn(){try{let e=await mt();W.value=e,We(e.agents||[]);let t=localStorage.getItem(cn),n=V.value.find(e=>e.is_default),r=V.value[0],i=V.value.find(e=>e.id===t)||n||r;i&&(B.value=i.id,F.value=Qt(i.id),localStorage.setItem(cn,i.id),await hn(i.id))}catch(e){throw console.error(`[boot] failed:`,e),e}}async function mn(){try{return(await R(null,{includeDms:!0})).sessions||[]}catch(e){return console.error(`[fetchCrossAgentSurfaces] failed:`,e),[]}}async function hn(e,t){let n=++Y;try{let[r,i]=await Promise.all([R(e,{includeDms:!1}),mn()]);if(n!==Y)return;let a=rn(r.sessions||[]);g(a,i),A([...a,...i]),Ot(e);let o=t?null:await fn(e,a);if(n!==Y)return;if(o)C.value=o,un(e,o),await Yt(o,{isStale:()=>n!==Y,logPrefix:`loadAgentSessions:hidden`});else if(a.length>0){let r=dn(e,a,t);C.value=r.id,un(e,r.id),await Yt(r.id,{isStale:()=>n!==Y,logPrefix:`loadAgentSessions`})}else{let t=`web-chat-`+Date.now(),r=await ke(e,t);if(n!==Y)return;let[i,a]=await Promise.all([R(e,{includeDms:!1}),mn()]);if(n!==Y)return;g(rn(i.sessions||[]),a),C.value=r.session_id,Qe([],r.session_id),Ke(r.session_id,[]),Ie(r.session_id)}}catch(e){if(n!==Y)return;console.error(`[loadAgentSessions] failed:`,e)}}async function gn(e,t){if(!V.value.find(t=>t.id===e))return;Re(),At(),at(),B.value=e,F.value=Qt(e),localStorage.setItem(cn,e),vt.value=!0,C.value=null,Ye.value=null,h(),Qe([]),N.value=[],G.value=null,K.value=null,Se();let n=hn(e,t&&t.targetSessionId),r=Y;try{await n}finally{r===Y&&(vt.value=!1)}}var X=s(null),Z=s(`agents`);function _n(e){X.value===e?X.value=null:(X.value=e,Z.value=e)}var vn=`alms_theme`;function yn(){return localStorage.getItem(vn)||`dark`}var bn=s(yn());function xn(){let e=bn.value===`dark`?`light`:`dark`;bn.value=e,localStorage.setItem(vn,e),document.documentElement.setAttribute(`data-theme`,e)}document.documentElement.setAttribute(`data-theme`,yn());var Sn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="8"/><path d="M10 6v4l3 3"/></svg>`,Cn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 5a2 2 0 012-2h3l2 2h5a2 2 0 012 2v7a2 2 0 01-2 2H5a2 2 0 01-2-2V5z"/></svg>`,wn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M5 5l10 10M15 5L5 15"/></svg>`,Tn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 15V5M10 5L5 10M10 5l5 5"/></svg>`,En=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor"><rect x="5" y="5" width="10" height="10" rx="1.5"/></svg>`,Dn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l10 6-10 6V4z"/></svg>`,On=()=>f`<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 00-2 2v.18a2 2 0 01-1 1.73l-.43.25a2 2 0 01-2 0l-.15-.08a2 2 0 00-2.73.73l-.22.38a2 2 0 00.73 2.73l.15.1a2 2 0 011 1.72v.51a2 2 0 01-1 1.74l-.15.09a2 2 0 00-.73 2.73l.22.38a2 2 0 002.73.73l.15-.08a2 2 0 012 0l.43.25a2 2 0 011 1.73V20a2 2 0 002 2h.44a2 2 0 002-2v-.18a2 2 0 011-1.73l.43-.25a2 2 0 012 0l.15.08a2 2 0 002.73-.73l.22-.39a2 2 0 00-.73-2.73l-.15-.08a2 2 0 01-1-1.74v-.5a2 2 0 011-1.74l.15-.09a2 2 0 00.73-2.73l-.22-.38a2 2 0 00-2.73-.73l-.15.08a2 2 0 01-2 0l-.43-.25a2 2 0 01-1-1.73V4a2 2 0 00-2-2z"/><circle cx="12" cy="12" r="3"/></svg>`,kn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M3 5h14M3 10h14M3 15h14"/></svg>`,An=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="4"/><path d="M10 2v2M10 16v2M3.5 10H2M18 10h-1.5M5.05 5.05L3.63 3.63M16.37 16.37l-1.42-1.42M5.05 14.95l-1.42 1.42M16.37 3.63l-1.42 1.42"/></svg>`,jn=()=>f`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 12.5A7.5 7.5 0 017.5 3 7.5 7.5 0 1017 12.5z"/></svg>`,Mn=s(!1);function Nn(){Mn.value=!Mn.value}function Pn(){Mn.value=!1}var Fn=[`agents`,`jobs`,`audit`],In=c(()=>W.value.posture||`guarded`);function Ln({onOpenSettings:e,status:t}){let n=In.value,r=t.value===`connected`?`ok`:t.value===`running`?`running`:t.value===`error`||t.value===`offline`?`error`:``;return f`
        <header>
            <button class="sidebar-toggle-btn" title="Toggle sessions" aria-label="Toggle sessions"
                    onClick=${Nn}>
                ${Mn.value?f`<${wn} />`:f`<${kn} />`}
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
            ${yt.value&&f`
                <button class="retry-btn" onClick=${St}>Retry</button>
            `}

            <div class="header-btns">
                ${Fn.map(e=>f`
                    <button class="hbtn ${X.value===e?`active`:``}"
                            onClick=${()=>_n(e)}>
                        ${e.charAt(0).toUpperCase()+e.slice(1)}
                    </button>
                `)}
            </div>

            <button class="header-icon-btn" title="Toggle theme" aria-label="Toggle theme"
                    onClick=${xn}>
                ${bn.value===`dark`?f`<${An} />`:f`<${jn} />`}
            </button>

            <button class="header-icon-btn settings-btn" title="Settings" aria-label="Settings"
                    onClick=${e}>
                <${On} />
            </button>
        </header>
    `}async function Rn(e,n){if(!e||e===C.value)return;Pn();let r=y.value.find(t=>t.id===e)||I.value.find(t=>t.id===e);if(r&&r.session_type!==`dm`&&r.session_type!==`notification`&&r.session_type!==`job`&&r.agent_id&&B.value&&r.agent_id!==B.value&&V.value.some(e=>e.id===r.agent_id)){await gn(r.agent_id,{targetSessionId:e});return}let i=at();Re(),t(()=>{C.value=e,Ge(),Ye.value=null,Qe([],e),N.value=[],K.value=null,Se(),L.value=null,_t.value=!0}),un(B.value,e);try{await Yt(e,{isStale:()=>i!==it,logPrefix:n&&n.logPrefix||`navigateToSession`})}finally{i===it&&(_t.value=!1)}}var zn=`alms.composer.draft.`,Bn=`alms.composer.queue.`,Vn=65536,Hn=50,Un=262144;function Wn(){try{return typeof window<`u`&&!!window.localStorage}catch{return!1}}function Gn(e){return zn+e}function Kn(e){return Bn+e}function qn(e){if(!e||!Wn())return``;try{let t=localStorage.getItem(Gn(e));return typeof t==`string`?t:``}catch{return``}}function Jn(e,t){if(e&&Wn()){if(t)try{localStorage.setItem(Gn(e),t)}catch{try{let n=t.length>Vn?t.slice(0,Vn):t;localStorage.setItem(Gn(e),n),n.length<t.length&&console.warn(`[composer-storage] draft truncated from %d to %d chars on save (storage quota)`,t.length,n.length)}catch{console.warn(`[composer-storage] draft not persisted (storage rejected both full and truncated writes); in-memory textarea is still authoritative`)}}else try{localStorage.removeItem(Gn(e))}catch{}}}function Yn(e){if(e&&Wn())try{localStorage.removeItem(Gn(e))}catch{}}function Xn(e){if(!e||!Wn())return[];try{let t=localStorage.getItem(Kn(e));if(!t)return[];let n=JSON.parse(t);return Array.isArray(n)?n.slice(0,Hn).filter(e=>e&&typeof e.text==`string`).map(e=>({text:e.text})):[]}catch{return[]}}function Zn(e,t){if(e&&Wn())try{if(!Array.isArray(t)||t.length===0){localStorage.removeItem(Kn(e));return}let n=t.slice(0,Hn).map(e=>({text:e.text})),r=JSON.stringify(n);for(;r.length>Un&&n.length>0;)n=n.slice(0,-1),r=JSON.stringify(n);if(n.length===0){localStorage.removeItem(Kn(e));return}localStorage.setItem(Kn(e),r)}catch{}}function Qn(e){if(e&&Wn())try{localStorage.removeItem(Kn(e))}catch{}}function $n(e){Yn(e),Qn(e)}function er(e,t,n){return!n||!Array.isArray(e)||e[0]!==t?e:e.slice(1)}function tr({restoredQueue:e,activeRunId:t,activeAgentId:n}){return!Array.isArray(e)||e.length===0?{drain:!1,head:null,remaining:[]}:t||!n?{drain:!1,head:null,remaining:e}:{drain:!0,head:e[0],remaining:e.slice(1)}}var nr={chat:{icon:`▸`,cls:``,label:`Chat session`},dm:{icon:`↔`,cls:`dm`,label:`DM conversation`},notification:{icon:`⚡`,cls:`notification`,label:`Notification session`},job:{icon:`⏰`,cls:`job`,label:`Job session`},subagent:{icon:`⚙`,cls:`subagent`,label:`Subagent session`},telegram:{icon:`✉`,cls:`telegram`,label:`Telegram session`}};function rr(e){return nr[e.session_type]||nr.chat}function ir(e,t,n,r){if(e===t&&n)return!0;let i=r[e];return!(!i||i.finished)}function ar(e){return Rn(e,{logPrefix:`selectSession`})}async function or(){if(B.value){Re(),at();try{let e=`web-chat-`+Date.now(),n=await ke(B.value,e),[r,i]=await Promise.all([R(B.value,{includeDms:!0}),mn()]);t(()=>{g(r.sessions||[],i),C.value=n.session_id,un(B.value,n.session_id),Ye.value=null,Qe([],n.session_id),N.value=[],Ke(n.session_id,[]),K.value=null,Se()}),Ie(n.session_id)}catch(e){console.error(`[newSession] failed:`,e)}}}function sr(e){let t=e.participants;return Array.isArray(t)&&t.length>=2?t.join(` <-> `):e.context_id||e.id.slice(0,8)}function cr(e){return e.agent_name?`notifications`:e.context_id||e.id.slice(0,8)}function lr(e){let t=e.context_id||``;return t.startsWith(`job_`)&&t.length>4?`job `+t.slice(4,12):t||e.id.slice(0,8)}function ur(e){return e.agent_name?`subagent`:e.context_id||e.id.slice(0,8)}function dr(e){return e.session_type===`dm`?sr(e):e.session_type===`notification`?cr(e):e.session_type===`job`?lr(e):e.session_type===`subagent`?ur(e):e.context_id||e.id.slice(0,8)}function fr(e){if(e.session_type===`notification`&&e.agent_name)return e.agent_name;if(e.session_type===`job`&&e.agent_id){let t=V.value.find(t=>t.id===e.agent_id);return t?t.name:null}if(e.session_type===`subagent`&&e.parent_agent_id){let t=V.value.find(t=>t.id===e.parent_agent_id);return t?t.name:null}return null}function pr(e,t){return e.session_type===`subagent`?`Invoked by `+t:`Owned by `+t}function mr({session:e,activeAgentName:n}){let r=o(!1),i=o(null),a=C.value,s=e.id===a,c=ir(e.id,a,U.value,E.value),l=rr(e),u=l.cls?` session-item-`+l.cls:``,d=e=>{e.stopPropagation(),r.value=!0,i.value=setTimeout(()=>{r.value=!1},3e3)},p=async n=>{n.stopPropagation(),i.value&&=(clearTimeout(i.value),null),r.value=!1;try{await me(e.id),$n(e.id),tt(e.id),Ge(e.id),e.id===C.value&&(Re(),t(()=>{C.value=null,Ye.value=null,K.value=null,Se(),N.value=[]}));let[n,r]=await Promise.all([R(B.value,{includeDms:!0}),mn()]);g(n.sessions||[],r)}catch(e){console.error(`[deleteSession] failed:`,e)}},m=e=>{e.stopPropagation(),i.value&&=(clearTimeout(i.value),null),r.value=!1},h=dr(e),_=e.session_type===`chat`?``:`
Type: `+e.session_type,v=fr(e),y=tn(e,n)?` session-item-active-agent`:``;return f`
        <div class="session-item${u}${y} ${s?`active`:``} ${c?`has-run`:``}"
             role="option"
             aria-selected=${s}
             tabindex="0"
             title=${`ID: `+e.id+`
Context: `+e.context_id+_}
             onClick=${()=>ar(e.id)}
             onKeyDown=${t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),ar(e.id))}}>
            ${e.session_type!==`chat`&&f`<span class="session-type-icon session-type-icon-${l.cls||`default`}" aria-hidden="true" title=${l.label}>${l.icon}</span>`}
            <span class="session-label">${h}</span>
            ${v&&f`<span class="session-agent-attribution" title=${pr(e,v)}>${v}</span>`}
            ${sn(e)&&(r.value?f`
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
                `)}
        </div>
    `}function hr({label:e,cls:t,id:n}){return f`
        <div class="session-section-divider ${t||``}" role="presentation" id=${n}>
            <span class="session-section-divider-label">${e}</span>
        </div>
    `}function gr({expanded:e,count:t,headerId:n}){let r=e=>{e.stopPropagation(),S.value=!S.value};return f`
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
    `}function _r(e){let t=Zt(F.value,e);F.value=t,t&&t!==B.value&&gn(t)}function vr({agent:e,expanded:t,sessionCount:n,isActive:r,headerId:i}){let a=t=>{t.stopPropagation(),_r(e.id)},o=t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),_r(e.id))},s=r?t?`Collapse sessions`:`Expand sessions`:`Switch to `+e.name;return f`
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
    `}function yr(e){let t=new Set,n=[];for(let r of e){if(r.session_type!==`dm`||!Array.isArray(r.participants)||r.participants.length<2)continue;let e=r.context_id||r.id;t.has(e)||(t.add(e),n.push(r))}return n}function br(){let e=y.value,t=I.value,n=V.value,r=B.value,i=F.value,a=H.value?H.value.name:null,o=en(e.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`&&e.session_type!==`subagent`&&e.session_type!==`episodic`)),s=en(on(t)),c=en(t.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`)),l=nn(yr(t),a),u=nn(t.filter(e=>e.session_type===`notification`),a),d=an(t),p=S.value,m=(!n||n.length===0)&&l.length===0&&u.length===0&&d.length===0;return f`
        <div class="sidebar-section" style="flex:1; min-height:0">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${m?f`<div class="empty-state">No sessions</div>`:null}
                ${n.map(e=>{let t=$t(i,e.id),n=e.id===r,l=[...o.get(e.id)||[],...s.get(e.id)||[]],u=n?l.length:(c.get(e.id)||[]).length,d=`agent-group-header-`+e.id;return f`
                        <div class="agent-group" key=${e.id}>
                            <${vr}
                                agent=${e}
                                expanded=${t}
                                sessionCount=${u}
                                isActive=${n}
                                headerId=${d} />
                            <div class="agent-group-body"
                                 role="group"
                                 aria-labelledby=${d}
                                 data-expanded=${t}>
                                <div class="agent-group-sessions">
                                    ${l.length===0?f`<div class="empty-state agent-group-empty">No sessions</div>`:l.map(e=>f`
                                            <${mr} key=${e.id} session=${e} activeAgentName=${a} />
                                        `)}
                                </div>
                            </div>
                        </div>
                    `})}
                ${l.length>0&&f`
                    <${hr} label="Direct messages"
                                       cls="session-divider-dm"
                                       id="session-section-dms" />
                    <div role="group" aria-labelledby="session-section-dms">
                        ${l.map(e=>f`
                            <${mr} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${u.length>0&&f`
                    <${hr} label="Notifications"
                                       cls="session-divider-notification"
                                       id="session-section-notifications" />
                    <div role="group" aria-labelledby="session-section-notifications">
                        ${u.map(e=>f`
                            <${mr} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${d.length>0&&f`
                    <${gr} expanded=${p}
                                          count=${d.length}
                                          headerId="session-section-jobs" />
                    <div class="agent-group-body"
                         role="group"
                         aria-labelledby="session-section-jobs"
                         data-expanded=${p}>
                        <div class="agent-group-sessions">
                            ${d.map(e=>f`
                                <${mr} key=${e.id} session=${e} activeAgentName=${a} />
                            `)}
                        </div>
                    </div>
                `}
            </div>
            <button id="new-session-btn" onClick=${or}>+ New session</button>
        </div>
    `}function xr(){let e=Mn.value?` sidebar-open`:``;return f`
        ${Mn.value&&f`<div class="sidebar-backdrop" onClick=${Pn}></div>`}
        <div id="sidebar" class=${e}>
            <${br} />
        </div>
    `}function Sr(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function Cr(e){if(!e)return``;let t=new Date(e),n=new Date;return t.toDateString()===n.toDateString()?Sr(e):t.toLocaleDateString([],{month:`short`,day:`numeric`})}function wr(e){if(!e)return``;let t=new Date(e);if(isNaN(t.getTime()))return``;let n=new Date,r=t.toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`});return t.toDateString()===n.toDateString()?r:`${t.toLocaleDateString([],{month:`short`,day:`numeric`})} ${r}`}function Tr(e){e&&(e.scrollTop=e.scrollHeight)}function Er(e){if(!e)return``;let t=``;if(typeof e.querySelector==`function`){let n=e.querySelector(`code`);n&&typeof n.textContent==`string`&&(t=n.textContent)}return!t&&typeof e.textContent==`string`&&(t=e.textContent),t?(t.endsWith(`\r
`)?t=t.slice(0,-2):t.endsWith(`
`)&&(t=t.slice(0,-1)),t):``}function Dr(){return!!(typeof navigator<`u`&&navigator.clipboard&&typeof navigator.clipboard.writeText==`function`)}var Or=`cb-copy-decorated`,kr=`code-block-wrapper`,Ar=`code-block-copy`,jr=`code-block-copy--copied`,Mr=`alms-code-copy-live`;function Nr(){if(typeof document>`u`)return null;let e=document.getElementById(Mr);return e||(e=document.createElement(`div`),e.id=Mr,e.setAttribute(`aria-live`,`polite`),e.setAttribute(`role`,`status`),e.style.position=`absolute`,e.style.width=`1px`,e.style.height=`1px`,e.style.padding=`0`,e.style.margin=`-1px`,e.style.overflow=`hidden`,e.style.clip=`rect(0, 0, 0, 0)`,e.style.whiteSpace=`nowrap`,e.style.border=`0`,document.body.appendChild(e),e)}function Pr(){let e=Nr();e&&(e.textContent=``,setTimeout(()=>{e.textContent=`Copied to clipboard`},50))}function Fr(){return`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="7" y="7" width="10" height="10" rx="1.5"/><path d="M5 13H4a1 1 0 01-1-1V4a1 1 0 011-1h8a1 1 0 011 1v1"/></svg>`}function Ir(){return`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 10l4 4 8-8"/></svg>`}function Lr(e){if(typeof document>`u`)return!1;let t=document.createElement(`textarea`);t.value=e,t.style.position=`fixed`,t.style.top=`-9999px`,t.style.left=`-9999px`,t.setAttribute(`readonly`,``),t.setAttribute(`aria-hidden`,`true`),document.body.appendChild(t);let n=!1;try{t.select(),t.setSelectionRange(0,e.length),n=document.execCommand&&document.execCommand(`copy`)}catch{n=!1}return document.body.removeChild(t),!!n}function Rr(e){e&&(e._copyRevertTimer&&=(clearTimeout(e._copyRevertTimer),null),e.classList.add(jr),e.innerHTML=Ir(),e.setAttribute(`aria-label`,`Copied`),e.title=`Copied`,Pr(),e._copyRevertTimer=setTimeout(()=>{e.classList.remove(jr),e.innerHTML=Fr(),e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e._copyRevertTimer=null},1500))}function zr(e,t,n){e.preventDefault(),e.stopPropagation();let r=Er(t);r&&(Dr()?navigator.clipboard.writeText(r).then(()=>Rr(n),()=>{Lr(r)&&Rr(n)}):Lr(r)&&Rr(n))}function Br(e,t=`pre`){if(!e||typeof e.querySelectorAll!=`function`)return;let n=e.querySelectorAll(`.${kr}`);for(let e=0;e<n.length;e++){let t=n[e];if(!t.parentNode)continue;let r=t.querySelector(`pre`);if(!r)t.parentNode.removeChild(t);else if(!r.classList.contains(Or)){let e=t.parentNode,n=Array.from(t.childNodes);for(let r=0;r<n.length;r++){let i=n[r];i.nodeType===1&&i.classList&&i.classList.contains(Ar)||e.insertBefore(i,t)}e.removeChild(t)}}let r=e.querySelectorAll(t);for(let e=0;e<r.length;e++){let t=r[e];if(t.classList.contains(Or))continue;let n=t.parentNode;if(!n)continue;if(n.classList&&n.classList.contains(kr)){if(!n.querySelector(`.${Ar}`)){let e=document.createElement(`button`);e.type=`button`,e.className=Ar,e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e.innerHTML=Fr(),e.addEventListener(`click`,n=>zr(n,t,e)),n.appendChild(e)}t.classList.add(Or);continue}if(!((t.textContent||``).trim().length>0)){t.classList.add(Or);continue}let i=document.createElement(`div`);i.className=kr,n.insertBefore(i,t),i.appendChild(t);let a=document.createElement(`button`);a.type=`button`,a.className=Ar,a.setAttribute(`aria-label`,`Copy code`),a.title=`Copy code`,a.innerHTML=Fr(),a.addEventListener(`click`,e=>zr(e,t,a)),i.appendChild(a),t.classList.add(Or)}}function Vr({ts:e}){if(!e)return null;let t=wr(e);return t?f`<span class="msg-timestamp" title=${e}>${t}</span>`:null}function Hr({text:e,live:t}){let n=o(!1);if(!e)return null;let r=()=>{n.value=!n.value},i=e.length>0?` (${e.length} chars)`:``,a=t?`Thinking…`:`Reasoning`,s=n.value?`▼`:`▶`;return f`
        <div class="reasoning-panel ${t?`reasoning-panel--live`:``} ${n.value?`reasoning-panel--open`:``}">
            <button class="reasoning-panel-toggle" onClick=${r}
                    aria-expanded=${n.value}>
                <span class="reasoning-panel-arrow">${s}</span>
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
    `}function Ur({html:e}){let t=l(null);return a(()=>{Br(t.current)},[e]),f`
        <div class="msg-body markdown-body" ref=${t}
             dangerouslySetInnerHTML=${{__html:e}} />
    `}function Wr(e,t){let n=re.value;return e?`>`:t?`${t} $`:n?`${n} $`:`$`}function Gr({type:e,role:t,text:n,sealed:r,fromAgent:i,reasoning:a,ts:o}){let s=e===`user`?`user`:`agent`,c=Wr(e===`user`,i),l=e===`agent`&&r===!1,u=e===`agent`&&a?f`<${Hr} text=${a} live=${l} />`:null,p=typeof n==`string`&&n.trim().length>0,m=o&&!l;if(e===`agent`&&r){let e=p?d(n):``;return f`
            <div class="msg ${s}">
                <div class="msg-label-row">
                    <div class="msg-label">${c}</div>
                    ${m&&f`<${Vr} ts=${o} />`}
                </div>
                ${u}
                ${p&&f`<${Ur} html=${e} />`}
            </div>
        `}return f`
        <div class="msg ${s}">
            <div class="msg-label-row">
                <div class="msg-label">${c}</div>
                ${m&&f`<${Vr} ts=${o} />`}
            </div>
            ${u}
            ${(p||l)&&f`
                <div class="msg-body ${l?`streaming-cursor`:``}">${n}</div>
            `}
        </div>
    `}function Kr({role:e,fromAgent:t,ts:n,url:r,alt:i}){let a=e===`user`&&!t,o=a?`user`:`agent`,s=Wr(a,t);return f`
        <div class="msg ${o}">
            <div class="msg-label-row">
                <div class="msg-label">${s}</div>
                ${n&&f`<${Vr} ts=${n} />`}
            </div>
            <div class="msg-body">
                ${r?f`<img src=${r} alt=${i||``} style="max-width:100%;border-radius:8px;" />`:`[Image${i?`: `+i:``}]`}
                ${i&&f`<div style="font-size:var(--text-xs);color:var(--text-secondary);margin-top:var(--space-2);">${i}</div>`}
            </div>
        </div>
    `}function qr({pending:e,queuedBehind:t,source:n}){let r=`Thinking`,i=`thinking-indicator`;e?(r=`Sending`,i=`pending-indicator`):t>0?(r=`Queued \u2014 position ${t}`,i=`queued-indicator`):n&&n.startsWith(`peer:`)?r=`Replying to message from `+n.slice(5):n===`job`?r=`Running scheduled job`:n===`subagent`&&(r=`Processing subagent result`);let a=re.value||`Agent`;return f`
        <div class="msg agent">
            <div class="msg-label">${a} $</div>
            <div class="msg-body ${i}">${r}</div>
        </div>
    `}function Jr({usage:e}){if(!e)return null;let t=e.prompt_tokens||0,n=e.completion_tokens||0;if(t+n===0)return null;let r=e.reasoning_tokens,i=typeof r==`number`&&r>0?` + ${r}r`:``;return f`<div class="msg-tokens">${t}p + ${n}c${i} tokens</div>`}function Yr({text:e,code:t}){let n=t?`msg-error--${t.toLowerCase()}`:``;return f`
        <div class="msg msg-error ${n}" data-code=${t||``}>
            <div class="msg-error-icon">\u274C</div>
            <div class="msg-error-body">
                <div class="msg-error-title">Error</div>
                <div class="msg-error-text">${e}</div>
            </div>
        </div>
    `}function Xr({id:e,text:t,code:n}){let r=o(!1),i=o(!1);if(i.value)return null;let a=()=>{r.value=!r.value},s=t=>{t.stopPropagation(),i.value=!0,e&&$e(t=>t.id!==e)},c=r.value?`msg-warning--collapsed`:``;return f`
        <div class="msg msg-warning ${c}" data-code=${n||``}>
            <div class="msg-warning-icon">\u26A0\uFE0F</div>
            <div class="msg-warning-body">
                <div class="msg-warning-header" onClick=${a}>
                    <div class="msg-warning-title">Warning</div>
                    ${n&&f`<span class="msg-warning-code">${n}</span>`}
                    <button class="msg-warning-toggle"
                            title=${r.value?`Expand`:`Collapse`}
                            aria-label=${r.value?`Expand warning`:`Collapse warning`}
                            aria-expanded=${!r.value}>
                        ${r.value?`▶`:`▼`}
                    </button>
                    <button class="msg-warning-dismiss" onClick=${s}
                            title="Dismiss" aria-label="Dismiss warning">
                        \u2715
                    </button>
                </div>
                ${!r.value&&f`
                    <div class="msg-warning-text">${t}</div>
                `}
            </div>
        </div>
    `}function Zr({text:e}){return f`
        <div class="msg-system">
            ${e}
        </div>
    `}function Qr({status:e,error:t}){if(!e||e===`completed`)return f`<div class="run-boundary run-boundary--completed" />`;let n=e===`failed`?`run-boundary--failed`:e===`cancelled`?`run-boundary--cancelled`:``,r=e===`failed`?`run failed`:e===`cancelled`?`run cancelled`:`run ${e}`;return f`
        <div class="run-boundary ${n}">
            <span class="run-boundary-label">${r}</span>
        </div>
        ${e===`failed`&&t&&f`
            <div class="run-boundary-error">${t}</div>
        `}
    `}function $r({peer:e,reason:t,detail:n}){return f`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">DM conversation with ${e} ended</span>
            <span class="dm-ended-reason">${t}</span>
            ${n&&f`<span class="dm-ended-detail">${n}</span>`}
        </div>
    `}function ei(e,t){if(!t)return``;switch(e){case`shell`:case`shell_exec`:return t.command?t.command:t.argv?t.argv.join(` `):``;case`fs_read`:return t.path||``;case`fs_write`:return`${t.mode===`append`?`(append) `:``}${t.path||``}`;case`fs_list`:return t.path||`.`;case`workspace_write`:return`${t.file||``}: ${(t.content||``).slice(0,60)}`;case`http_get`:if(!t.url)return``;try{return new URL(t.url).hostname+` `+t.url}catch{return t.url}case`math`:return t.operation?t.operation+`(`+[t.a,t.b,t.n].filter(e=>e!==void 0).join(`, `)+`)`:``;case`echo`:return t.message||t.text||``;case`send_message`:return t.to?`to ${t.to}`:``;case`invoke_agent`:{let e=t.name||t.subagent_name||``,n=t.task||``;return e&&n?`${e}: ${n.length>60?n.slice(0,60)+`…`:n}`:e}case`read_session`:return(t.session_id?t.session_id.slice(0,8)+`…`:``)+(t.last_n?` (last ${t.last_n})`:``);case`read_subagent_session`:return(t.name||``)+(t.last_n?` (last ${t.last_n})`:``);case`list_agents`:case`list_my_sessions`:return``;case`read_messages`:return t.from?`from ${t.from}`:``;case`ignore_message`:return t.from?`from ${t.from}`:``;default:{let e=Object.entries(t);return e.map(([t,n])=>{let r=typeof n==`string`?n:JSON.stringify(n);return e.length>1?`${t}=${r}`:r}).join(` `)}}}function ti(e){return e<1024?e+` B`:e<1048576?(e/1024).toFixed(1)+` KB`:(e/1048576).toFixed(1)+` MB`}var ni=2e3,ri=800;function ii(e){if(!e)return``;let t=e.replace(/\\/g,`/`).split(`/`).filter(Boolean);return t.length<=2?t.join(`/`):`…/`+t.slice(-2).join(`/`)}function Q(e){return typeof e==`object`&&!!e&&typeof e.error==`string`}function ai(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.task_id==`string`?e.task_id:null,n=typeof e.status==`string`?e.status:null,r=typeof e.command==`string`?e.command:null,i=typeof e.exit_code==`number`?e.exit_code:null,a=typeof e.stdout==`string`?e.stdout:``,o=typeof e.stderr==`string`?e.stderr:``,s=typeof e.error==`string`?e.error:null,c=typeof e.message==`string`?e.message:null;return t&&(n===`submitted`||n===`unknown`||n===`not_found_or_still_running`)?f`
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
    `}function oi(e,t){if(typeof e!=`object`||!e||Q(e)||typeof e.content!=`string`)return null;let n=t&&t.path||``,r=typeof e.lines_returned==`number`?e.lines_returned:null,i=typeof e.total_lines==`number`?e.total_lines:null,a=e.has_more_before===!0,o=e.has_more_after===!0,s=typeof e.note==`string`?e.note:null,c=e.byte_budget_exceeded===!0,l=e.line_truncated===!0,u=[];r!=null&&i!=null?r===i?u.push(`${r} lines (full file)`):u.push(`${r} of ${i} lines`):r!=null&&u.push(`${r} lines`),a&&u.push(`more before`),o&&u.push(`more after`),c&&u.push(`byte-budget exceeded`),l&&u.push(`per-line truncated`);let d=u.join(` · `),p=e.content||``;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">
                ${n?ii(n):`File content`}
            </div>
            ${p?f`<pre class="tc-detail-content tc-code-block">${p}</pre>`:f`<pre class="tc-detail-content tc-detail-muted">${s||`(empty)`}</pre>`}
            ${d&&f`<div class="tc-detail-footer">${d}</div>`}
            ${p&&s&&f`<div class="tc-detail-footer">${s}</div>`}
        </div>
    `}function si(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.path==`string`&&e.path||typeof e.file==`string`&&e.file||null,n=typeof e.replacements==`number`?e.replacements:null,r=typeof e.mode==`string`?e.mode:null,i=e.ok===!0;return t?f`
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
    `:null}function ci(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.matches))return null;let t=e.matches,n=e.truncated===!0,r=typeof e.truncated_lines==`number`&&e.truncated_lines>0?e.truncated_lines:0;if(t.length===0)return f`
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
    `}function li(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.files))return null;let t=e.files,n=typeof e.total==`number`?e.total:t.length,r=e.truncated===!0;if(t.length===0)return f`
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
    `}function ui(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.entries))return null;let t=typeof e.path==`string`?e.path:``,n=e.entries;return n.length===0?f`
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
    `}function di(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.status==`number`?e.status:null,a=typeof e.content_type==`string`?e.content_type:null,o=e.body;if(i==null&&o===void 0)return null;let s=a&&a.toLowerCase().includes(`application/json`),c;if(typeof o==`string`)c=o;else if(o==null)c=``;else try{c=JSON.stringify(o,null,2)}catch{c=String(o)}let l=c.length>ni,u=r&&!r.value&&l?c.slice(0,ni)+`…`:c,d=e=>{e.stopPropagation(),r&&(r.value=!r.value)},p=i!=null&&i>=200&&i<400?`tc-kv-badge`:`tc-kv-badge tc-kv-badge-fail`,m=[];if(e.headers&&typeof e.headers==`object`&&!Array.isArray(e.headers)){let t=Object.keys(e.headers).sort();for(let n of t){let t=e.headers[n];if(Array.isArray(t))for(let e of t)m.push([n,typeof e==`string`?e:String(e)]);else typeof t==`string`&&m.push([n,t])}}let h=m.length;return f`
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
    `}function fi(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.task_id==`string`?e.task_id:null,a=typeof e.session_id==`string`?e.session_id:null,o=typeof e.response==`string`?e.response:``,s=t&&(t.name||t.subagent_name)||``;if(i)return f`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Subagent (background)</div>
                <div class="tc-status-row">
                    ${s&&f`<span class="tc-kv-badge">${s}</span>`}
                    <span class="tc-kv-mono">task_id: ${i}</span>
                </div>
                ${a&&f`
                    <button class="tc-detail-link"
                        type="button"
                        onClick=${e=>{e.stopPropagation(),Rn(a,{logPrefix:`invokeAgentLink`})}}>
                        View full session
                    </button>
                `}
            </div>
        `;if(!o&&!a)return null;let c=o.length>ri,l=r&&!r.value&&c?o.slice(0,ri)+`…`:o;return f`
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
                    onClick=${e=>{e.stopPropagation(),Rn(a,{logPrefix:`invokeAgentLink`})}}>
                    View full session
                </button>
            </div>
        `}
    `}function pi(e){if(typeof e!=`object`||!e||Q(e))return null;let t=e.delivered===!0,n=typeof e.dm_session_id==`string`?e.dm_session_id:null,r=typeof e.note==`string`?e.note:null;return!t&&!n?null:f`
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
    `}function mi(e,t,n){let r=n&&n.tool;if(typeof e!=`object`||!e||Q(e))return null;let i=Array.isArray(e.messages)?e.messages:null,a=typeof e.summary==`string`&&e.summary.length>0?e.summary:null,o=typeof e.peer==`string`?e.peer:null,s=typeof e.subagent==`string`?e.subagent:null,c=typeof e.session_id==`string`?e.session_id:null,l=typeof e.note==`string`&&e.note.length>0?e.note:null,u=typeof e.message_count==`number`?e.message_count:typeof e.fallback_message_count==`number`?e.fallback_message_count:null,d=typeof e.showing==`number`?e.showing:typeof e.fallback_showing==`number`?e.fallback_showing:i?i.length:null;if(!i&&a){let e=[];return u!=null&&e.push(`${u} messages total`),c&&e.push(`session: ${c.slice(0,8)}…`),f`
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
    `}function hi(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.agents))return null;let t=e.agents;return t.length===0?f`
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
    `}function gi(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.sessions))return null;let t=e.sessions,n=typeof e.total==`number`?e.total:t.length,r=typeof e.showing==`number`?e.showing:t.length;if(t.length===0)return f`
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
    `}function _i(e){if(Q(e))return null;let t;if(typeof e==`string`)t=e;else if(e&&typeof e==`object`)try{t=JSON.stringify(e,null,2)}catch{return null}else if(typeof e==`number`||typeof e==`boolean`)t=String(e);else return null;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Echoed</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `}function vi(e){return Q(e)||typeof e!=`number`?null:f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${e}</span>
            </div>
        </div>
    `}function yi(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.iso==`string`?e.iso:null,n=typeof e.human==`string`?e.human:null,r=typeof e.timezone==`string`?e.timezone:null,i=typeof e.local_iso==`string`?e.local_iso:null,a=typeof e.local_human==`string`?e.local_human:null,o=typeof e.local_timezone==`string`?e.local_timezone:null,s=typeof e.utc_offset==`string`?e.utc_offset:null;return!t&&!i?null:f`
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
    `}function bi(e){if(typeof e!=`object`||!e||Q(e)||e.ignored!==!0)return null;let t=typeof e.reason==`string`?e.reason:``;return f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Ignored</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">ignored</span>
                ${t&&f`<span class="tc-kv-meta">${t}</span>`}
            </div>
        </div>
    `}function xi(e,t,n,r){if(t==null)return null;switch(e){case`shell`:case`shell_exec`:return ai(t);case`fs_read`:return oi(t,n);case`fs_write`:case`workspace_write`:case`fs_edit`:return si(t);case`fs_grep`:return ci(t);case`fs_glob`:return li(t);case`fs_list`:return ui(t);case`http_get`:return di(t,n,r);case`invoke_agent`:return fi(t,n,r);case`send_message`:return pi(t);case`read_messages`:case`read_session`:case`read_subagent_session`:return mi(t,n,{...r,tool:e});case`list_agents`:return hi(t);case`list_my_sessions`:return gi(t);case`ignore_message`:return bi(t);case`echo`:return _i(t);case`math`:return vi(t);case`datetime`:return yi(t);default:return null}}var Si=500,Ci=200;function wi(e,t){return typeof e==`string`?e.length<=t?e:e.slice(0,t)+`…`:``}var Ti={fs_edit:[`old_string`,`new_string`],invoke_agent:[`task`],send_message:[`message`],ignore_message:[`reason`],echo:[`message`,`text`]};function Ei(e,t){if(!t||typeof t!=`object`)return!1;let n=Ti[e];if(!n)return!1;for(let e of n){let n=t[e];if(typeof n==`string`&&n.length>Ci)return!0}return!1}function Di(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function Oi(e){if(e==null)return``;if(typeof e==`string`)try{let t=JSON.parse(e);return JSON.stringify(t,null,2)}catch{return e}return JSON.stringify(e,null,2)}function ki(e){if(e==null)return 0;let t=typeof e==`string`?e:JSON.stringify(e);return new Blob([t]).size}function Ai(e){switch(e){case`shell`:case`shell_exec`:return`$`;case`fs_read`:return`R`;case`fs_write`:return`W`;case`fs_list`:return`L`;case`workspace_write`:return`W`;case`http_get`:return`H`;case`send_message`:return`DM`;case`invoke_agent`:return`IA`;case`read_session`:case`read_subagent_session`:return`RS`;case`list_agents`:return`LA`;case`list_my_sessions`:return`LS`;case`read_messages`:return`RM`;case`ignore_message`:return`IG`;case`math`:return`#`;case`echo`:return`E`;default:return`T`}}function ji(e,t){if(!t)return null;switch(e){case`shell`:case`shell_exec`:if(t.command)return f`
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
                        <pre class="tc-detail-content tc-code-block">${wi(t.old_string,Ci)}</pre>
                    </div>
                `}
                ${t.new_string&&f`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Replace with</div>
                        <pre class="tc-detail-content tc-code-block">${wi(t.new_string,Ci)}</pre>
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
                        <pre class="tc-detail-content">${wi(t.task,Ci)}</pre>
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
                `;break;case`workspace_write`:{let e=typeof t.mode==`string`?t.mode:null;return f`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Workspace</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${t.file||``}</span>
                        ${e&&f`<span class="tc-kv-meta">${e}</span>`}
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
                        <pre class="tc-detail-content">${wi(t.message,Ci)}</pre>
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
                    ${e?f`<pre class="tc-detail-content">${wi(e,Ci)}</pre>`:f`<div class="tc-detail-footer">no reason given</div>`}
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
                    <pre class="tc-detail-content">${wi(t.message||t.text||``,Ci)}</pre>
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
            `}}let n=Oi(t);return n?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters</div>
            <pre class="tc-detail-content">${n}</pre>
        </div>
    `:null}function Mi({params:e}){let t=Oi(e);return t?f`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters (raw)</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `:null}function Ni({tool:e,params:t,panelRef:n}){let r=o(!1);a(()=>{Br(n?.current,`pre.tc-code-block`)});let i=ji(e,t);return i?Ei(e,t)?f`
        ${r.value?f`<${Mi} params=${t} />`:i}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),r.value=!r.value}}>
                ${r.value?`Hide raw params`:`View raw params`}
            </button>
        </div>
    `:i:null}function Pi({result:e,isFail:t,showFull:n,label:r,blockedTarget:i}){let a=Oi(e);if(!a)return null;let o=a.length>Si,s=!n.value&&o?a.slice(0,Si)+`…`:a,c=n.value?` tc-detail-expanded`:``;return f`
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
    `}function Fi({tool:e,params:t,result:n,isFail:r,isCancelled:i,showFull:s,panelRef:c}){let l=o(!1);if(a(()=>{Br(c?.current,`pre.tc-code-block`)}),n==null&&!r)return null;let u=r&&typeof n==`object`&&n&&typeof n.target==`string`&&n.target.length>0?n.target:null;if(r)return f`<${Pi} result=${n} isFail=${!0}
            showFull=${s} label="Error" blockedTarget=${u} />`;if(i)return f`<${Pi} result=${n} isFail=${!1}
            showFull=${s} label="Result (cancelled)" />`;let d=xi(e,n,t,{showFull:s});return d?f`
        ${l.value?f`<${Pi} result=${n} isFail=${!1}
                showFull=${s} label="Result (raw)" />`:d}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),l.value=!l.value}}>
                ${l.value?`Hide raw`:`View raw`}
            </button>
        </div>
    `:f`<${Pi} result=${n} isFail=${!1}
            showFull=${s} label="Result" />`}function Ii({tool:e,params:t,status:n,result:r,id:i,sourceAgent:s,durationMs:c}){let u=o(!1),d=o(!1),p=e=>{e.stopPropagation(),u.value=!u.value},m=l(null);a(()=>{u.value&&Br(m.current,`pre.tc-code-block`)});let h=ei(e,t),g=h.length>80?h.slice(0,80)+`…`:h,_=n===`running`,v=n===`fail`,y=n===`done`,b=n===`cancelled`,x=e===`send_message`,S=v?`tc-fail`:y?`tc-done`:b?`tc-cancelled`:`tc-running`,C=u.value?`▼`:`▶`,w=Ai(e),ee=Di(c),T=r==null?0:ki(r),E=T>=100?ti(T):``;return f`
        <div class="tc-row ${S} ${x?`tc-dm`:``}" role="button" tabindex="0"
             onClick=${p} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),p(e))}}>
            <div class="tc-header">
                <span class="tc-chevron">${C}</span>
                ${_?f`<span class="tc-spinner"></span>`:f`<span class="tc-icon">${w}</span>`}
                <span class="tc-name">${e}</span>
                ${g&&f`<span class="tc-summary">${g}</span>`}
                <span class="tc-spacer"></span>
                ${E&&f`<span class="tc-result-size">${E}</span>`}
                ${ee&&f`<span class="tc-duration">${ee}</span>`}
                ${v&&f`<span class="tc-status-badge tc-badge-fail">failed</span>`}
                ${b&&f`<span class="tc-status-badge tc-badge-cancelled">cancelled</span>`}
                ${y&&f`<span class="tc-status-icon">\u2713</span>`}
            </div>
            ${u.value&&f`
                <div class="tc-detail" ref=${m}
                     onClick=${e=>e.stopPropagation()}>
                    ${f`<${Ni} tool=${e} params=${t}
                        panelRef=${m} />`}
                    ${f`<${Fi} tool=${e} params=${t}
                        result=${r} isFail=${v} isCancelled=${b}
                        showFull=${d} panelRef=${m} />`}
                </div>
            `}
        </div>
    `}function Li({children:e,count:t}){return t<=1?e:f`
        <div class="tc-group">
            <div class="tc-group-label">${t} tools in parallel</div>
            ${e}
        </div>
    `}function Ri(e,t){return e?e.length<=t?e:e.slice(0,t)+`...`:``}function zi(e){switch(e){case`system`:return`cd-role-system`;case`user`:return`cd-role-user`;case`assistant`:return`cd-role-assistant`;case`tool`:return`cd-role-tool`;default:return``}}function Bi(e){return e==null?`--`:Number(e).toLocaleString()}function Vi({msg:e,index:t}){let n=o(!1),r=e.role||`unknown`,i=e.content||``,a=e.tool_calls&&e.tool_calls.length>0,s=!!e.tool_call_id,c=Ri(i,120),l=`[${t}] ${r}`;if(s&&(l+=` (tool_result)`),a){let t=e.tool_calls.map(e=>e.function?.name||`?`).join(`, `);l+=` -> ${t}`}return f`
        <div class="cd-msg" role="button" tabindex="0"
             onClick=${e=>{e.stopPropagation(),n.value=!n.value}}
             onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),e.stopPropagation(),n.value=!n.value)}}>
            <div class="cd-msg-header">
                <span class="cd-msg-chevron">${n.value?`▼`:`▶`}</span>
                <span class="cd-msg-role ${zi(r)}">${r}</span>
                ${!n.value&&c&&f`<span class="cd-msg-preview">${c}</span>`}
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
    `}function Hi({messages:e,toolNames:t,totalTokens:n,systemTokens:r,historyMessageCount:i,agentName:a,agentId:s}){let c=o(!1),l=e=>{e.stopPropagation(),c.value=!c.value},u=Array.isArray(e)?e.length:0,d=a?`Context sent to LLM (${a})`:`Context sent to LLM`;return f`
        <div class="cd-row" role="button" tabindex="0"
             onClick=${l} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),l(e))}}>
            <div class="cd-header">
                <span class="cd-chevron">${c.value?`▼`:`▶`}</span>
                <span class="cd-icon">CTX</span>
                <span class="cd-title">${d}</span>
                <span class="cd-stats">
                    ${Bi(n)} tokens | ${u} messages | ${(t||[]).length} tools
                </span>
            </div>
            ${c.value&&f`
                <div class="cd-detail" onClick=${e=>e.stopPropagation()}>
                    <!-- Token breakdown -->
                    <div class="cd-section">
                        <div class="cd-section-label">Token breakdown</div>
                        <div class="cd-token-grid">
                            <span class="cd-token-label">System prompt:</span>
                            <span class="cd-token-value">${Bi(r)}</span>
                            <span class="cd-token-label">History messages:</span>
                            <span class="cd-token-value">${i}</span>
                            <span class="cd-token-label">Total estimated:</span>
                            <span class="cd-token-value cd-token-total">${Bi(n)}</span>
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
                        <div class="cd-section-label">Messages (${u})</div>
                        <div class="cd-messages">
                            ${(e||[]).map((e,t)=>f`
                                <${Vi} key=${t} msg=${e} index=${t} />
                            `)}
                        </div>
                    </div>
                </div>
            `}
        </div>
    `}async function Ui(e,t){try{await v(`/approvals/${e}`,{decision:t})}catch(e){throw console.error(`[resolveApproval] failed:`,e),e}}function Wi({approvalId:e,tool:t,params:n}){let r=o(!1),i=async()=>{if(!r.value){r.value=!0;try{await Ui(e,`approve`)}catch{r.value=!1}}},a=async()=>{if(!r.value){r.value=!0;try{await Ui(e,`deny`)}catch{r.value=!1}}},s=r.value;return f`
        <div class="approval-card">
            <h3>\u26a0 Approval required \u2014 ${t}</h3>
            <pre>${JSON.stringify(n,null,2)}</pre>
            <div class="approval-btns">
                <button class="btn btn-approve" onClick=${i} disabled=${s}>
                    ${s?`Submitting...`:`Approve`}
                </button>
                <button class="btn btn-deny" onClick=${a} disabled=${s}>
                    ${s?`Submitting...`:`Deny`}
                </button>
            </div>
        </div>
    `}function Gi(e){return typeof e==`string`&&e.endsWith(`...`)}function Ki({runId:e,summary:t,truncated:n}={}){return e?typeof n==`boolean`?n:Gi(t):!1}function qi({jobSessionUuid:e}={}){return typeof e==`string`&&e.length>0?e:null}function Ji(e,t){return typeof t==`string`&&t.length>0?t:e||``}var Yi=150;function Xi(e){if(!e)return``;try{return new Date(e).toLocaleTimeString(void 0,{hour:`2-digit`,minute:`2-digit`})}catch{return``}}function Zi(e){switch(e){case`success`:return`Completed`;case`error`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Finished`}}function Qi(e){switch(e){case`success`:return`✓`;case`error`:return`✗`;case`cancelled`:return`–`;default:return`•`}}function $i({jobName:e,status:t,summary:n,ts:r,runId:i,truncated:s,jobSessionUuid:c,jobSessionId:l}){let u=o(!1),p=o(null),m=o(!1),h=n&&n.length>Yi,g=!h||u.value;a(()=>{if(!u.value||p.value!==null||m.value||!Ki({runId:i,summary:n,truncated:s}))return;m.value=!0;let e=!1;return dt(i).then(t=>{e||(p.value=Ji(n,t&&t.response))}).catch(()=>{e||(p.value=n||``)}).finally(()=>{e||(m.value=!1)}),()=>{e=!0}},[u.value,i,n,s]);let _=p.value==null?n:p.value,v=`job-card--${t||`success`}`,y=Xi(r),b=Qi(t),x=Zi(t),S=()=>{u.value=!u.value},C=qi({jobSessionUuid:c}),w=e=>{e.stopPropagation(),C&&Rn(C,{logPrefix:`job-card`})},ee=_?d(_):``;return f`
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
                                     dangerouslySetInnerHTML=${{__html:ee}} />`:f`<div class="job-card-summary-truncated">
                                ${n.slice(0,Yi)}...
                            </div>`}
                </div>
            `}
            ${(h||C)&&f`
                <div class="job-card-actions">
                    ${h&&f`
                        <button class="job-card-toggle" onClick=${S}>
                            ${u.value?`Show less`:`Show more`}
                        </button>
                    `}
                    ${C&&f`
                        <button class="job-card-goto" onClick=${w}>
                            Go to job session →
                        </button>
                    `}
                </div>
            `}
        </div>
    `}var ea=200;function ta(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function na(e){switch(e){case`done`:return`Completed`;case`fail`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Completed`}}function ra(e){switch(e){case`done`:return`✓`;case`fail`:return`✗`;case`cancelled`:return`–`;default:return`✓`}}function ia({name:e,task:t,status:n,toolCount:r,durationMs:i,sessionId:a,summary:s}){let c=o(!1),l=`sa-card--${n||`done`}`,u=ra(n),d=na(n),p=ta(i),m=s&&s.length>ea,h=!c.value&&m?s.slice(0,ea)+`…`:s;return f`
        <div class="sa-card ${l}">
            <div class="sa-card-header">
                <span class="sa-card-icon">${u}</span>
                <span class="sa-card-badge">${d}</span>
                <span class="sa-card-label">Subagent</span>
                ${p&&f`<span class="sa-card-meta">${p}</span>`}
                ${r>0&&f`<span class="sa-card-meta">${r} tool${r===1?``:`s`}</span>`}
            </div>
            <div class="sa-card-name">${e||`subagent`}</div>
            ${t&&f`<div class="sa-card-task">${t}</div>`}
            ${s&&f`
                <div class="sa-card-body">
                    <div class="sa-card-summary">${h}</div>
                    ${m&&f`
                        <button class="sa-card-toggle" onClick=${()=>{c.value=!c.value}}>
                            ${c.value?`Show less`:`Show more`}
                        </button>
                    `}
                </div>
            `}
            ${a&&f`
                <div class="sa-card-actions">
                    <button class="sa-card-view-btn" onClick=${e=>{e.stopPropagation(),a&&ve(a)}}>
                        View session \u2192
                    </button>
                </div>
            `}
        </div>
    `}function aa(e){let t=N.value.filter((t,n)=>n!==e);N.value=t,Zn(C.value,t)}function oa(){let e=N.value;return e.length===0?null:f`
        <div id="message-queue">
            ${e.map((e,t)=>f`
                <div class="queued-msg">
                    <span class="queued-msg-label">queued</span>
                    <span class="queued-msg-text">${e.text}</span>
                    <button class="queued-msg-remove" title="Remove from queue"
                            onClick=${()=>aa(t)}>\u00d7</button>
                </div>
            `)}
        </div>
    `}async function sa(e,t){let n=t?.sessionId||C.value,r=B.value;if(!r)return n?et(e=>[...e,{id:P(),type:`error`,text:`Select an agent before sending a message.`}],n):console.warn(`[startRun] rejected: no agent or session selected`),!1;if(!n)return console.warn(`[startRun] rejected: no session selected`),!1;if(ae(n).length>0)return!1;let i=new Date().toISOString(),a={id:P(),type:`user`,role:`user`,text:e,ts:i};oe(n,e,a,[{id:P(),type:`thinking`,pending:!0}]);try{let i=await lt({session_id:n,agent_id:r,input:{type:`text`,text:e}});n&&i?.run_id&&(w(n,a.id,i.run_id),!t?.queued&&!U.value&&N.value.length>0&&await ca(N.value[0],n))}catch(e){ie(n,{messageId:a.id}),et(t=>[...t.filter(e=>e.type!==`thinking`),{id:P(),type:`error`,text:`Failed to start run: ${e.error?.message||e.message||e.status||`unknown error`}`}],n),console.error(`[startRun] failed:`,e)}return!0}async function ca(e,t){let n=N.value;if(n[0]!==e)return!1;let r=await sa(e.text,{sessionId:t,queued:!0}),i=N.value,a=er(i,e,r);if(a===i){let i=er(n,e,r);return i!==n&&(Zn(t,i),!0)}return N.value=a,Zn(t,a),!U.value&&a.length>0&&await ca(a[0],t),!0}function la(e){let t=e.current.value.trim();if(!t||!C.value||!B.value)return;let n=C.value;if(!(!U.value&&ae(n).length>0)){if(e.current.value=``,e.current.style.height=`auto`,Yn(n),U.value){let r=[...N.value,{text:t}];N.value=r,Zn(n,r),e.current.focus()}else sa(t)}}async function ua(){if(U.value)try{await pt(U.value)}catch{}}function da(e){e.style.height=`auto`,e.style.height=Math.min(e.scrollHeight,150)+`px`}function fa(){let e=l(null),t=V.value.length>0,n=!!C.value,r=!!B.value,i=t&&r&&n,o=!!U.value,s=r?`Send a message...`:`Select an agent to send a message`,c=C.value;return a(()=>{let t=e.current;t&&(t.value=qn(c),da(t));let n=Xn(c),r=tr({restoredQueue:n,activeRunId:U.value,activeAgentId:B.value});n.length>0&&(N.value=n),r.drain&&ca(r.head,c).catch(e=>{console.error(`[queue] mount drain failed:`,e)})},[c]),f`
        <div id="input-area">
            <div class="input-container">
                <textarea id="prompt" ref=${e} rows="1"
                          placeholder=${s}
                          aria-label="Message input"
                          disabled=${!i}
                          onKeyDown=${t=>{t.key===`Enter`&&!t.shiftKey&&(t.preventDefault(),la(e))}}
                          onInput=${()=>{let t=e.current;t&&(da(t),Jn(c,t.value))}}></textarea>
                ${o?f`<button id="cancel-run" title="Stop run" aria-label="Stop run"
                                   onClick=${ua}><${En} /></button>`:f`<button id="send" disabled=${!i}
                                   title="Send (Enter)" aria-label="Send message"
                                   onClick=${()=>la(e)}><${Tn} /></button>`}
            </div>
        </div>
    `}var pa=()=>D(`/agents`),ma=e=>v(`/agents`,e),ha=(e,t)=>b(`/agents/${e}`,t),ga=e=>p(`/agents/${e}`),_a=e=>v(`/agents/${e}/default`),va={"claude-opus-4-7":{name:`Claude Opus 4.7`,provider:`anthropic`},"claude-opus-4-6":{name:`Claude Opus 4.6`,provider:`anthropic`},"claude-sonnet-4-6":{name:`Claude Sonnet 4.6`,provider:`anthropic`},"claude-sonnet-4-5":{name:`Claude Sonnet 4.5`,provider:`anthropic`},"claude-haiku-4-5":{name:`Claude Haiku 4.5`,provider:`anthropic`},"gpt-5.4":{name:`GPT-5.4`,provider:`openai`},"gpt-5.4-mini":{name:`GPT-5.4 mini`,provider:`openai`},"gpt-5.4-nano":{name:`GPT-5.4 nano`,provider:`openai`},"gpt-4.1":{name:`GPT-4.1`,provider:`openai`},"gpt-4.1-mini":{name:`GPT-4.1 mini`,provider:`openai`},"gpt-4.1-nano":{name:`GPT-4.1 nano`,provider:`openai`},"gpt-4o":{name:`GPT-4o`,provider:`openai`},"gpt-4o-mini":{name:`GPT-4o mini`,provider:`openai`},"o4-mini":{name:`o4-mini`,provider:`openai`},o3:{name:`o3`,provider:`openai`},"o3-mini":{name:`o3-mini`,provider:`openai`},"grok-4.20":{name:`Grok 4.20`,provider:`xai`},"grok-4-fast":{name:`Grok 4 Fast`,provider:`xai`},"grok-3":{name:`Grok 3`,provider:`xai`},"grok-3-mini":{name:`Grok 3 mini`,provider:`xai`},"deepseek-chat":{name:`DeepSeek Chat (V3)`,provider:`deepseek`},"deepseek-reasoner":{name:`DeepSeek Reasoner (R1)`,provider:`deepseek`},"mistral-large-latest":{name:`Mistral Large`,provider:`mistral`},"mistral-medium-latest":{name:`Mistral Medium`,provider:`mistral`},"mistral-small-latest":{name:`Mistral Small`,provider:`mistral`},"codestral-latest":{name:`Codestral`,provider:`mistral`},"ministral-8b-latest":{name:`Ministral 8B`,provider:`mistral`},"open-mistral-nemo":{name:`Mistral Nemo`,provider:`mistral`},"llama-3.3-70b-versatile":{name:`Llama 3.3 70B`,provider:`groq`},"llama-3.1-8b-instant":{name:`Llama 3.1 8B (Instant)`,provider:`groq`},"deepseek-r1-distill-llama-70b":{name:`DeepSeek R1 Distill (70B)`,provider:`groq`},"qwen-2.5-32b":{name:`Qwen 2.5 32B`,provider:`groq`},"qwen2.5-coder:32b":{name:`Qwen 2.5 Coder 32B`,provider:`ollama`},"deepseek-r1:7b":{name:`DeepSeek R1 7B`,provider:`ollama`},"llama3.3:70b":{name:`Llama 3.3 70B`,provider:`ollama`},"deepseek/deepseek-r1":{name:`DeepSeek R1`,provider:`openrouter`},"deepseek/deepseek-chat-v3-0324":{name:`DeepSeek Chat v3`,provider:`openrouter`},"z-ai/glm-5.2":{name:`GLM 5.2`,provider:`openrouter`},"z-ai/glm-5.1":{name:`GLM 5.1`,provider:`openrouter`},"minimax/minimax-m2.7":{name:`MiniMax M2.7`,provider:`openrouter`},"xiaomi/mimo-v2-pro":{name:`MiMo v2-pro`,provider:`openrouter`},"moonshotai/kimi-k2.6":{name:`Kimi K2.6`,provider:`openrouter`},"google/gemma-4-31b-it":{name:`Gemma 4 31B`,provider:`openrouter`}},ya=`claude-opus-4-7,claude-sonnet-4-6,claude-haiku-4-5,claude-opus-4-6,gpt-5.4,gpt-5.4-mini,gpt-5.4-nano,gpt-4.1,gpt-4.1-mini,gpt-4o,gpt-4o-mini,o4-mini,o3,grok-4.20,grok-4-fast,grok-3-mini,deepseek-chat,deepseek-reasoner,mistral-large-latest,mistral-small-latest,codestral-latest,llama-3.3-70b-versatile,llama-3.1-8b-instant,deepseek-r1-distill-llama-70b,qwen2.5-coder:32b,deepseek-r1:7b,llama3.3:70b,z-ai/glm-5.2,deepseek/deepseek-r1,deepseek/deepseek-chat-v3-0324,z-ai/glm-5.1,minimax/minimax-m2.7,xiaomi/mimo-v2-pro,moonshotai/kimi-k2.6,google/gemma-4-31b-it`.split(`,`);function ba(e){if(!e)return``;let t=va[e];return t?t.name:e}function xa(e){if(!e)return`unknown`;let t=va[e];return t?t.provider:e.includes(`/`)?`openrouter`:e.includes(`:`)?`ollama`:e.startsWith(`claude`)?`anthropic`:e.startsWith(`gpt`)||/^o\d/.test(e)?`openai`:e.startsWith(`grok`)?`xai`:e.startsWith(`deepseek-`)?`deepseek`:e.startsWith(`mistral-`)||e.startsWith(`codestral-`)||e.startsWith(`ministral-`)||e.startsWith(`open-mistral-`)||e.startsWith(`open-mixtral-`)?`mistral`:e.startsWith(`llama-`)?`groq`:e.startsWith(`gemini-`)?`google`:`unknown`}var Sa={anthropic:`Anthropic`,openai:`OpenAI`,openrouter:`OpenRouter`,xai:`xAI`,deepseek:`DeepSeek`,mistral:`Mistral`,groq:`Groq`,ollama:`Ollama`,google:`Google`,unknown:`Custom`};function Ca(e){return e?Sa[e]?Sa[e]:Sa[xa(e)]:Sa.unknown}function wa({modelId:e,provider:t}){let n=t||xa(e),r=Sa[n]||Sa.unknown;return f`
        <span class="model-provider-badge model-provider-badge--${n}"
              title=${`Provider: ${r}`}>${r}</span>
    `}function Ta({value:e,defaultValue:t,showBadge:n=!0}){let r=e&&e.trim?e.trim():e,i=!!r&&r!==t,a=r||t;if(!a)return f`<span class="model-display model-display--muted">unknown</span>`;let o=ba(a),s=o===a?a:`${o} (${a})`;return i?f`
            <span class="model-display" title=${s}>
                <span class="model-override-pill" title="Per-run override">override</span>
                <span class="model-name">${o}</span>
                ${n&&f`<${wa} modelId=${a} />`}
            </span>
        `:f`
        <span class="model-display model-display--default" title=${s}>
            <span class="model-default-label">Default</span>
            <span class="model-name">${o}</span>
            ${n&&f`<${wa} modelId=${a} />`}
        </span>
    `}function Ea({value:e,defaultValue:t}){let n=e&&e.trim?e.trim():e,r=!!n&&n!==t,i=n||t;return i?r?f`
            <span class="model-display">
                <span class="model-override-pill" title="Per-run override">override</span>
                <${wa} provider=${i} />
            </span>
        `:f`
        <span class="model-display model-display--default">
            <span class="model-default-label">Default</span>
            <${wa} provider=${i} />
        </span>
    `:f`<span class="model-display model-display--muted">unknown</span>`}function Da(e){return e===`Enter`||e===` `}function Oa(e){return!e||typeof e.key!=`string`||e.defaultPrevented?!1:Da(e.key)}function ka(e){return!(!e||e.defaultPrevented)}function Aa(e,t){let n=!!e,r=!!t;return n===r?{}:{debug_mode:r}}var ja=`Emits a snapshot of the context window sent to the LLM on every turn, rendered below the chat. Applies from the next run.`;function Ma(e){return`Each ${e} applies only when the agent is on that provider; the others are ignored.`}var Na=`Optional cheaper model for compaction summaries and episodic memory. Set provider and model together, or neither.`,Pa=[`minimal`,`low`,`medium`,`high`];function Fa(e){return e==null?`inherit`:e===0?`disable`:`custom`}function Ia(e){return e==null||e===``?`inherit`:`custom`}function La({label:e,agentValue:t,mode:n,draft:r}){return f`
        <div class="settings-row">
            <label class="settings-label">${e}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${n.value}
                        onChange=${e=>{n.value=e.target.value,n.value!==`custom`&&(r.value=``)}}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="disable">Disable (0)</option>
                    <option value="custom">Custom</option>
                </select>
                ${n.value===`custom`&&f`
                    <input class="settings-input agent-tristate-value"
                           type="number" min="0" step="1024"
                           placeholder="tokens"
                           value=${r.value}
                           onInput=${e=>{r.value=e.target.value}} />
                `}
            </div>
        </div>
    `}function Ra({label:e,mode:t,value:n}){return f`
        <div class="settings-row">
            <label class="settings-label">${e}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${t.value}
                        onChange=${e=>{t.value=e.target.value,t.value!==`custom`&&(n.value=``)}}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="custom">Custom</option>
                </select>
                ${t.value===`custom`&&f`
                    <select class="settings-select agent-tristate-value"
                            value=${n.value}
                            onChange=${e=>{n.value=e.target.value}}>
                        <option value="">choose...</option>
                        ${Pa.map(e=>f`<option value=${e} key=${e}>${e}</option>`)}
                    </select>
                `}
            </div>
        </div>
    `}async function za(){try{let e=await pa();We(e.agents||e||[])}catch(e){console.error(`[agents] fetch failed:`,e)}}function Ba({agent:e,onClose:t}){let n=o(e.description||``),r=o(e.model||``),i=o(e.posture||``),a=o(e.provider||``),s=o(`keep`),c=o(``),l=o(Fa(e.thinking_budget_tokens)),u=o(e.thinking_budget_tokens&&e.thinking_budget_tokens>0?String(e.thinking_budget_tokens):``),d=o(Ia(e.reasoning_effort)),p=o(e.reasoning_effort||``),m=o(Fa(e.gemini_thinking_budget)),h=o(e.gemini_thinking_budget&&e.gemini_thinking_budget>0?String(e.gemini_thinking_budget):``),g=o(e.summary_provider||``),_=o(e.summary_model||``),v=o(!!e.debug_mode),y=o(!1),b=o(``),x=W.value.model||``,S=W.value.provider||``,C=W.value.llm_providers||[],w=()=>{let t={};n.value!==(e.description||``)&&(t.description=n.value),(r.value||``)!==(e.model||``)&&(t.model=r.value||``),(i.value||``)!==(e.posture||``)&&(t.posture=i.value||``),(a.value||``)!==(e.provider||``)&&(t.provider=a.value||``),s.value===`set`&&c.value.trim()?t.telegram_token=c.value.trim():s.value===`remove`&&(t.telegram_token=``);let o=e.thinking_budget_tokens;if(l.value===`inherit`)o!=null&&(t.clear_thinking_budget_tokens=!0);else if(l.value===`disable`)o!==0&&(t.thinking_budget_tokens=0);else if(l.value===`custom`){let e=parseInt(u.value,10);!isNaN(e)&&e>=0&&e!==o&&(t.thinking_budget_tokens=e)}let f=e.reasoning_effort||null;d.value===`inherit`?f!=null&&(t.clear_reasoning_effort=!0):d.value===`custom`&&p.value&&p.value!==f&&(t.reasoning_effort=p.value);let y=e.gemini_thinking_budget;if(m.value===`inherit`)y!=null&&(t.clear_gemini_thinking_budget=!0);else if(m.value===`disable`)y!==0&&(t.gemini_thinking_budget=0);else if(m.value===`custom`){let e=parseInt(h.value,10);!isNaN(e)&&e>=0&&e!==y&&(t.gemini_thinking_budget=e)}let b=e.summary_provider||``,x=(g.value||``).trim();x!==b&&(x===``?t.clear_summary_provider=!0:t.summary_provider=x);let S=e.summary_model||``,C=(_.value||``).trim();return C!==S&&(C===``?t.clear_summary_model=!0:t.summary_model=C),Object.assign(t,Aa(e.debug_mode,v.value)),t},ee=async()=>{y.value=!0,b.value=``;try{let n=w();if(Object.keys(n).length===0){t();return}await ha(e.id,n),await za(),t()}catch(e){b.value=e.error?.message||e.message||`Save failed`}finally{y.value=!1}},T=e=>{e.target===e.currentTarget&&t()},E=!!e.has_telegram;return f`
        <div class="settings-overlay open" onClick=${T}>
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
                        ${ya.map(e=>f`<option value=${e} />`)}
                    </datalist>
                    <span class="settings-effective">
                        Effective: <${Ta} value=${r.value.trim()} defaultValue=${x} />
                    </span>
                    <span class="settings-hint">Leave empty to use server default.</span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Provider</label>
                    <select class="settings-select"
                            value=${a.value}
                            onChange=${e=>{a.value=e.target.value}}>
                        <option value="">Default (${Ca(S||`openai`)})</option>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="openrouter">OpenRouter</option>
                    </select>
                    <span class="settings-effective">
                        Effective: <${Ea} value=${a.value} defaultValue=${S||`openai`} />
                    </span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Posture</label>
                    <select class="settings-select"
                            value=${i.value}
                            onChange=${e=>{i.value=e.target.value}}>
                        <option value="">Server default (${W.value.posture||`guarded`})</option>
                        <option value="full_control">full_control</option>
                        <option value="guarded">guarded</option>
                        <option value="autonomous">autonomous</option>
                    </select>
                </div>

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Reasoning &amp; thinking</div>

                <span class="settings-hint">${Ma(`row`)}</span>

                <!-- No per-row hints: the tri-state option labels already read
                     "Inherit (server default)" / "Disable (0)" / "Custom". -->
                <${La}
                    label="Anthropic thinking budget"
                    agentValue=${e.thinking_budget_tokens}
                    mode=${l}
                    draft=${u} />

                <${Ra}
                    label="OpenAI reasoning effort"
                    mode=${d}
                    value=${p} />

                <${La}
                    label="Gemini thinking budget"
                    agentValue=${e.gemini_thinking_budget}
                    mode=${m}
                    draft=${h} />

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Summary</div>

                <span class="settings-hint">
                    ${Na} Leave both empty to use the server default.
                </span>

                <div class="settings-row">
                    <label class="settings-label">Summary provider</label>
                    <select class="settings-select"
                            value=${g.value}
                            onChange=${e=>{g.value=e.target.value}}>
                        <option value="">Use server default</option>
                        ${(C.length>0?C:[`openai`,`anthropic`,`openrouter`,`gemini`]).map(e=>{let t=Ca(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                    </select>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Summary model</label>
                    <input class="settings-input" type="text"
                           list="agent-model-suggestions"
                           placeholder="(use server default)"
                           value=${_.value}
                           onInput=${e=>{_.value=e.target.value}} />
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
                    <span class="settings-hint">${ja}</span>
                </div>

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Telegram bot</div>

                <div class="settings-row">
                    <label class="settings-label">Token</label>
                    <div class="settings-info-row-header" style="flex-wrap:wrap;gap:6px;">
                        <span class="settings-info-row-value">
                            ${E?`configured (token hidden)`:`not configured`}
                        </span>
                        <select class="settings-select agent-tristate-mode"
                                value=${s.value}
                                onChange=${e=>{s.value=e.target.value,e.target.value!==`set`&&(c.value=``)}}>
                            <option value="keep">Keep</option>
                            <option value="set">${E?`Replace`:`Set`}</option>
                            ${E&&f`<option value="remove">Remove</option>`}
                        </select>
                    </div>
                    ${s.value===`set`&&f`
                        <input class="settings-input" type="password"
                               autocomplete="off"
                               placeholder="paste bot token..."
                               value=${c.value}
                               onInput=${e=>{c.value=e.target.value}} />
                    `}
                    <span class="settings-hint">
                        ${E?`The stored token is never displayed.`:`Set a bot token to give this agent its own Telegram bot.`}
                        ${` `}Takes effect after a daemon restart.
                    </span>
                </div>

                ${b.value&&f`<div class="inline-error">${b.value}</div>`}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${t}>Cancel</button>
                    <button class="settings-save" onClick=${ee} disabled=${y.value}>
                        ${y.value?`...`:`Save`}
                    </button>
                </div>
            </div>
        </div>
    `}function Va({agent:e,isActive:t,onEdit:n}){let r=o(``),i=o(!1),a=o(null),s=W.value.model||``,c=W.value.provider||``;return f`
        <div class="agent-card ${t?`active`:``}"
             role="option"
             tabindex="0"
             aria-label=${`Select agent `+e.name}
             aria-selected=${t?`true`:`false`}
             onClick=${t=>{ka(t)&&gn(e.id)}}
             onKeyDown=${t=>{Oa(t)&&(t.preventDefault(),gn(e.id))}}>
            <div class="agent-card-header">
                <span class="agent-card-name">${e.name}</span>
                ${e.is_default&&f`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta agent-card-meta--model">
                <span class="agent-card-meta-label">model:</span>
                <${Ta} value=${e.model} defaultValue=${s} />
            </div>
            ${e.provider&&f`
                <div class="agent-card-meta agent-card-meta--provider">
                    <span class="agent-card-meta-label">provider:</span>
                    <${Ea} value=${e.provider} defaultValue=${c} />
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
                    <button class="agent-card-btn" onClick=${async t=>{t&&t.stopPropagation();try{await _a(e.id),await za()}catch(e){r.value=e.error?.message||e.message||`Failed`}}}>Set Default</button>
                `}
                ${i.value?f`
                        <button class="agent-card-btn" style="color:var(--error); font-weight:600;" onClick=${async t=>{t&&t.stopPropagation(),a.value&&=(clearTimeout(a.value),null),i.value=!1;try{if(await ga(e.id),await za(),e.id===B.value){let e=V.value.find(e=>e.is_default)||V.value[0]||null;e?gn(e.id):(B.value=null,F.value=null)}}catch(e){r.value=e.error?.message||e.message||`Delete failed`}}}>Confirm?</button>
                        <button class="agent-card-btn" onClick=${e=>{e&&e.stopPropagation(),a.value&&=(clearTimeout(a.value),null),i.value=!1}}>Cancel</button>
                    `:f`<button class="agent-card-btn" style="color:var(--error);" onClick=${e=>{e&&e.stopPropagation(),i.value=!0,a.value=setTimeout(()=>{i.value=!1},3e3)}}>Delete</button>`}
            </div>
        </div>
    `}function Ha(){let e=o(``),t=o(``),n=o(!1),r=o(null);a(()=>{Z.value===`agents`&&za()},[Z.value]);let i=It(e.value),s=(e.value||``).trim(),c=s!==``&&i!==s,l=async()=>{let r=It(e.value);if(!r){(e.value||``).trim()===``?t.value=`Agent name is required`:t.value=`Agent name must contain at least one letter or digit`;return}let i=Bt(r);if(i)t.value=i.message;else{t.value=``,n.value=!0;try{let t=await ma({name:r});t.id||console.warn(`[agents] POST /agents returned no id for agent:`,r,t),e.value=``,await za()}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}}};return f`
        <div class="agent-list-container">
            <div class="agent-create-row">
                <input type="text" placeholder="New agent name..."
                       aria-label="Agent name"
                       value=${e.value}
                       onInput=${t=>{e.value=t.target.value}}
                       onKeyDown=${e=>{e.key===`Enter`&&l()}} />
                <button class="agent-card-btn agent-create-btn" onClick=${l}
                        disabled=${n.value}>
                    ${n.value?`...`:`+ Create`}
                </button>
            </div>
            ${c&&f`
                <div class="agent-create-preview">
                    Will be saved as: <code>${i}</code>
                </div>
            `}

            ${t.value&&f`<div class="agent-error">${t.value}</div>`}

            <div class="agent-list" role="listbox" aria-label="Agents">
                ${V.value.length===0?f`<div class="empty-state">No agents</div>`:V.value.map(e=>f`
                        <${Va} key=${e.id} agent=${e}
                                      isActive=${e.id===B.value}
                                      onEdit=${e=>{r.value=e}} />
                    `)}
            </div>

            ${r.value&&f`
                <${Ba}
                    agent=${r.value}
                    onClose=${()=>{r.value=null}} />
            `}
        </div>
    `}var Ua=e=>D(`/agents/${e}/workspace`),Wa=(e,t,n)=>b(`/agents/${e}/workspace/${t}`,{content:n}),Ga=e=>v(`/agents/${e}/workspace/open`,{}),Ka=[`personality`,`goals`,`memories`,`user`];async function qa(){if(!B.value)G.value=null;else try{let e=await Ua(B.value);G.value=e.files||e}catch(e){G.value=e.status===404||e.error?.code===`NOT_FOUND`?`unavailable`:`error`}}function Ja({agentId:e,doOpen:t}){let n=o(!1),r=o(null),i=async()=>{if(!n.value&&e){n.value=!0,r.value=null;try{await t(e),r.value={kind:`ok`,text:`Opened`},setTimeout(()=>{r.value?.kind===`ok`&&(r.value=null)},2e3)}catch(e){let t=e?.error?.code,n=e?.error?.message||e?.message||`Failed to open workspace`,i=n;t===`NOT_CONFIGURED`?i=`Workspace dir not configured`:t===`WORKSPACE_PATH_MISSING`?i=`Workspace path is missing on disk`:t===`LAUNCHER_FAILED`&&(i=`Failed to launch file explorer`),r.value={kind:`err`,text:i,full:n}}finally{n.value=!1}}};return f`
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
    `}function Ya({agentId:e,filename:t,content:n}){let r=o(n||``),i=o(``),s=o(!1);return a(()=>{r.value=n||``},[n]),f`
        <div class="ws-file">
            <div class="ws-file-label">${t}</div>
            <textarea class="ws-textarea"
                      rows="6"
                      value=${r.value}
                      onInput=${e=>{r.value=e.target.value}}></textarea>
            <div style="display:flex; align-items:center; gap:var(--space-2);">
                <button class="ws-save" onClick=${async()=>{if(!s.value){s.value=!0,i.value=``;try{await Wa(e,t,r.value),i.value=`Saved`,setTimeout(()=>{i.value=``},2e3),await qa()}catch(e){i.value=`Error: `+(e.error?.message||e.message||`save failed`)}finally{s.value=!1}}}} disabled=${s.value}>
                    ${s.value?`Saving...`:`Save`}
                </button>
                ${i.value&&f`
                    <span class="ws-flash ${i.value.startsWith(`Error`)?`err`:`ok`}">
                        ${i.value}
                    </span>
                `}
            </div>
        </div>
    `}function Xa(){return a(()=>{Z.value===`workspace`&&qa()},[Z.value,B.value]),B.value?G.value===null?f`<div class="loading-state">Loading...</div>`:G.value===`unavailable`?f`<div class="ws-notice">Workspace not configured for this agent</div>`:G.value===`error`?f`<div class="ws-notice" style="color:var(--error);">Failed to load workspace</div>`:f`
        <div>
            <${Ja}
                agentId=${B.value}
                doOpen=${Ga} />
            ${Ka.map(e=>f`
                <${Ya}
                    key=${e}
                    agentId=${B.value}
                    filename=${e}
                    content=${G.value[e+`.md`]||G.value[e]||``} />
            `)}
        </div>
    `:f`<div class="ws-notice">No agent selected</div>`}var Za=z.jobs;function Qa(){return z.getJobMutationGeneration()}function $a(e,t){z.replaceJobs(e,t)}function eo(e){z.createOptimisticJob(e)}function to(e,t){z.confirmOptimisticJobCreate(e,t)}function no(e){z.rollbackOptimisticJobCreate(e)}function ro(e){z.cancelOptimisticJob(e)}function io(e,t){z.confirmOptimisticJobCancel(e,t)}function ao(e){z.rollbackOptimisticJobCancel(e)}var oo=()=>D(`/jobs`),so=e=>v(`/jobs`,e),co=e=>p(`/jobs/${e}`),lo=0,uo=[{label:`1m`,cron:`* * * * *`,desc:`Every minute`},{label:`5m`,cron:`*/5 * * * *`,desc:`Every 5 minutes`},{label:`15m`,cron:`*/15 * * * *`,desc:`Every 15 minutes`},{label:`30m`,cron:`*/30 * * * *`,desc:`Every 30 minutes`},{label:`1h`,cron:`0 * * * *`,desc:`Every hour`},{label:`6h`,cron:`0 */6 * * *`,desc:`Every 6 hours`},{label:`12h`,cron:`0 */12 * * *`,desc:`Every 12 hours`},{label:`1d`,cron:`0 0 * * *`,desc:`Daily at midnight`}];function fo(e){return e===`pending`||e===`active`}function po(e){let t=e.status||`active`;return e.terminal_reason?`${t} (${e.terminal_reason.replaceAll(`_`,` `)})`:t}function mo(e){return e.retry_count?`${e.retry_count} dispatch ${e.retry_count===1?`retry`:`retries`}`:``}function ho(e){if(!e)return``;let t=uo.find(t=>t.cron===e.trim());return t?t.desc:e.trim().split(/\s+/).length===5?e:`Invalid cron (need 5 fields)`}function go(e){let t=e=>String(e).padStart(2,`0`);return`${e.getFullYear()}-${t(e.getMonth()+1)}-${t(e.getDate())}T${t(e.getHours())}:${t(e.getMinutes())}`}function _o(){let e=new Date(Date.now()+3e5);return e.setSeconds(0,0),go(e)}function vo(){let e=new Date;return e.setSeconds(0,0),go(e)}async function yo(){let e=Qa();try{let t=await oo();$a(t.jobs||t||[],e)}catch(e){console.error(`[jobs] fetch failed:`,e)}}function bo(){let e=o(`recurring`),t=o(``),n=o(_o()),r=o(``),i=o(B.value||``),s=o(``),c=o(``),l=o(!1),u=o(!1);a(()=>{Z.value===`jobs`&&yo()},[Z.value]),a(()=>{i.value=B.value||``},[B.value]);let d=ho(t.value),p=e.value===`once`?!!n.value:!!t.value.trim(),m=!!i.value&&p&&!!r.value.trim(),h=async()=>{if(!m)return;s.value=``,c.value=``,l.value=!0;let a=null;try{let o;if(e.value===`once`){let e=new Date(n.value);if(isNaN(e.getTime())){s.value=`Invalid date/time. Please select a valid date.`,l.value=!1;return}o={type:`once`,run_at:e.toISOString()}}else o={type:`recurring`,cron:t.value.trim()};let d={agent_id:i.value,schedule:o,prompt:r.value.trim()};a=`optimistic-job-`+ ++lo,eo({id:a,...d,status:`pending`,next_run_at:null,last_run_at:null});let f=await so(d);to(a,f),a=null,t.value=``,r.value=``,u.value=!1,n.value=_o(),c.value=e.value===`once`?`Job scheduled (one-time).`:`Recurring job created.`,setTimeout(()=>{c.value=``},4e3)}catch(e){a&&no(a);let t=e.error?.message||e.message||``;s.value=t||`Failed to create job. Check that all fields are filled and the schedule is valid.`}finally{l.value=!1}},g=async e=>{s.value=``,ro(e);try{io(e,await co(e))}catch(t){ao(e),t.status===409&&await yo(),s.value=t.error?.message||t.message||`Failed to cancel job`}};return f`
        <div>
            <div class="jobs-form">
                <select class="jobs-select" value=${i.value}
                        onChange=${e=>{i.value=e.target.value}}>
                    ${V.value.map(e=>f`
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
                        ${uo.map(e=>f`
                            <button class="cron-btn ${t.value===e.cron?`active`:``}"
                                    title=${e.desc}
                                    onClick=${()=>{t.value=e.cron,u.value=!1}}>
                                ${e.label}
                            </button>
                        `)}
                        <button class="cron-btn ${u.value?`active`:``}"
                                title="Custom cron expression"
                                onClick=${()=>{u.value=!0,t.value=``}}>
                            custom
                        </button>
                    </div>

                    ${u.value&&f`
                        <input class="jobs-input" type="text" placeholder="min hour dom mon dow"
                               value=${t.value}
                               onInput=${e=>{t.value=e.target.value}} />
                    `}

                    ${t.value&&f`
                        <div class="cron-preview">${d}</div>
                    `}
                `:f`
                    <input class="jobs-input" type="datetime-local"
                           value=${n.value}
                           min=${vo()}
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
                        disabled=${l.value||!m}>
                    ${l.value?`Scheduling...`:`Schedule`}
                </button>
            </div>

            ${c.value&&f`<div class="jobs-success">${c.value}</div>`}
            ${s.value&&f`<div class="jobs-error">${s.value}</div>`}

            <div class="jobs-divider"></div>

            ${Za.value.length===0?f`<div class="jobs-empty">No scheduled jobs</div>`:Za.value.map(e=>f`
                    <div class="job-item">
                        <div class="job-prompt">${e.prompt||e.task||`(no prompt)`}</div>
                        <div class="job-meta">
                            <span>${ho(e.schedule?.cron)||(e.schedule?.type===`once`?`Once at `+Cr(e.schedule.run_at):JSON.stringify(e.schedule))}</span>
                            ${e.next_run_at&&f`<span> | next: ${Cr(e.next_run_at)}</span>`}
                            ${e.last_run_at&&f`<span> | last run: ${Cr(e.last_run_at)}</span>`}
                            ${mo(e)&&f`<span> | ${mo(e)}</span>`}
                            ${e.last_error&&f`<span class="job-last-error"> | last error: ${e.last_error}</span>`}
                        </div>
                        <span class="job-status-${e.status||`active`}">${po(e)}</span>
                        ${fo(e.status||`active`)&&!e.optimistic&&f`
                            <button class="job-cancel" onClick=${()=>g(e.id)}>Cancel</button>
                        `}
                    </div>
                `)}
        </div>
    `}var xo=(e,t=50)=>D(`/audit?session_id=${e}&limit=${t}`),So=50;async function Co(e){if(!C.value)K.value=null;else try{let t=await xo(C.value,e);K.value=t.events||t||[]}catch{K.value=[]}}function wo(){let e=o(So),t=o(!1);a(()=>{Z.value===`audit`&&(e.value=So,Co(So))},[Z.value,C.value]);let n=async()=>{t.value=!0;try{let t=e.value+So;e.value=t,await Co(t)}catch(e){console.error(`[AuditTab] loadMore failed:`,e)}finally{t.value=!1}};if(!C.value)return f`<div class="empty-state">No session selected</div>`;if(K.value===null)return f`<div class="loading-state">Loading...</div>`;if(K.value.length===0)return f`<div class="empty-state">No audit events</div>`;let r=K.value.length>=e.value;return f`
        <div>
            ${K.value.map((e,t)=>f`
                <div class="audit-event" key=${e.id||`audit-${e.timestamp||``}-${t}`}>
                    <span class="audit-tool">${e.tool||e.action||`unknown`}</span>
                    <span class="${e.decision===`deny`?`audit-deny`:e.decision===`error`?`audit-error`:`audit-allow`}">
                        ${e.decision===`deny`?`denied`:e.decision===`error`?`error`:`allowed`}
                    </span>
                    ${e.timestamp&&f`<span class="audit-time">${Cr(e.timestamp)}</span>`}
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
    `}var To=50,Eo={completed:`✓`,failed:`✗`,cancelled:`⊘`,running:`⋯`},Do={user:`user`,scheduled:`scheduled`,subagent:`subagent`,dm:`dm`,notification:`notif`,telegram:`telegram`},Oo={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`};function ko(e){if(e==null)return`--`;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m`+(n>0?n+`s`:``)}function Ao(e){return e==null?`--`:e>=1e4?(e/1e3).toFixed(0)+`k`:e>=1e3?(e/1e3).toFixed(1)+`k`:String(e)}function jo(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function Mo(){let e=o([]),t=o(!1),n=o(``),r=async()=>{if(!B.value)e.value=[];else{t.value=!0,n.value=``;try{let t=await ut(B.value,To);e.value=t.runs||[]}catch(t){console.error(`[RunsTab] fetch failed:`,t),n.value=t.error?.message||t.message||`Failed to load runs`,e.value=[]}finally{t.value=!1}}};return a(()=>{Z.value===`runs`&&r()},[Z.value,B.value,qe.value]),B.value?t.value&&e.value.length===0?f`<div class="loading-state">Loading runs...</div>`:n.value?f`
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
                         onClick=${()=>e.session_id&&Rn(e.session_id)}
                         title=${`Run `+e.run_id.slice(0,8)+` | Session `+(e.session_id||``).slice(0,8)}>
                        <div class="runs-tab-row-top">
                            <span class="runs-tab-status">${Eo[e.status]||`·`}</span>
                            <span class="runs-tab-trigger runs-tab-trigger--${e.trigger||`user`}">
                                ${Do[e.trigger]||e.trigger||`user`}
                            </span>
                            <span class="runs-tab-session-type">
                                ${Oo[e.session_type]||e.session_type||``}
                            </span>
                            <span class="runs-tab-time">${jo(e.ts)}</span>
                        </div>
                        <div class="runs-tab-row-bottom">
                            <span class="runs-tab-duration">${ko(e.duration_ms)}</span>
                            <span class="runs-tab-tools">${e.tool_call_count==null?``:e.tool_call_count+` tools`}</span>
                            <span class="runs-tab-tokens">
                                ${e.usage?Ao(e.usage.prompt_tokens)+` in / `+Ao(e.usage.completion_tokens)+` out`+(typeof e.usage.reasoning_tokens==`number`&&e.usage.reasoning_tokens>0?` (+`+Ao(e.usage.reasoning_tokens)+` reasoning)`:``)+(typeof e.usage.cache_read_input_tokens==`number`&&e.usage.cache_read_input_tokens>0?` (`+Ao(e.usage.cache_read_input_tokens)+` cached)`:``):``}
                            </span>
                        </div>
                    </div>
                `)}
            </div>
        </div>
    `:f`<div class="runs-tab-empty">No agent selected</div>`}function No(e,t=50,n=null){let r=`/agents/${e}/timeline?limit=${t}`;return n&&(r+=`&before=${encodeURIComponent(n)}`),D(r)}var Po=50,Fo={run_started:`▶`,run_completed:`✓`,run_failed:`✗`,run_cancelled:`⊘`,run_ended:`■`,tool_call:`⚙`,message_received:`●`,message_sent:`○`,marker:`⚑`},Io={run_started:`started`,run_completed:`completed`,run_failed:`failed`,run_cancelled:`cancelled`,run_ended:`ended`,tool_call:`tool`,message_received:`message`,message_sent:`sent`,marker:`marker`},Lo={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`,episodic:`epis`};function Ro(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function zo(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function Bo(e){if(!e)return``;let t=new Date(e),n=new Date,r=new Date;return r.setDate(r.getDate()-1),t.toDateString()===n.toDateString()?`Today`:t.toDateString()===r.toDateString()?`Yesterday`:t.toLocaleDateString([],{weekday:`short`,month:`short`,day:`numeric`})}function Vo(){let e=o([]),t=o(!1),n=o(!1),r=o(``),i=o(!1),s=o(null),c=async(a=!1)=>{if(!B.value)e.value=[];else{a?n.value=!0:t.value=!0,r.value=``;try{let t=a?s.value:null,n=await No(B.value,Po,t),r=n.events||[];a?e.value=[...e.value,...r]:e.value=r,i.value=n.pagination?.has_more||!1,s.value=n.pagination?.next_before||null}catch(t){console.error(`[TimelineTab] fetch failed:`,t),r.value=t.error?.message||t.message||`Failed to load timeline`,a||(e.value=[])}finally{t.value=!1,n.value=!1}}};if(a(()=>{Z.value===`timeline`&&c(!1)},[Z.value,B.value]),!B.value)return f`<div class="tl-empty">No agent selected</div>`;if(t.value&&e.value.length===0)return f`<div class="loading-state">Loading timeline...</div>`;if(r.value)return f`
            <div>
                <div class="tl-error">${r.value}</div>
                <button class="tl-retry" onClick=${()=>c(!1)}>Retry</button>
            </div>
        `;if(e.value.length===0)return f`<div class="tl-empty">No activity yet</div>`;let l=new Set;{let t=``;for(let n of e.value){let e=Bo(n.timestamp);e!==t&&(l.add(n),t=e)}}return f`
        <div class="tl-tab">
            <div class="tl-header">
                <span class="tl-count">${e.value.length} event${e.value.length===1?``:`s`}</span>
                <button class="tl-refresh" onClick=${()=>c(!1)}
                        disabled=${t.value} title="Refresh">
                    ${t.value?`...`:`↻`}
                </button>
            </div>
            <div class="tl-list">
                ${e.value.map((e,t)=>{let n=Bo(e.timestamp),r=l.has(e),i=e.event_type===`tool_call`,a=e.event_type===`run_started`||e.event_type===`run_completed`||e.event_type===`run_failed`||e.event_type===`run_cancelled`||e.event_type===`run_ended`,o=e.metadata?.tool_name,s=e.event_type+`-`+e.timestamp+`-`+(e.run_id||``)+`-`+t+(o?`-`+o:``);return f`
                        ${r&&f`
                            <div class="tl-date-group" key=${`g-`+n}>${n}</div>
                        `}
                        <div class="tl-event tl-event--${e.event_type}${i?` tl-event--indent`:``}${a?` tl-event--run`:``}"
                             key=${s}
                             onClick=${()=>Rn(e.session_id)}
                             title=${`Session `+(e.session_id||``).slice(0,8)+(e.run_id?` | Run `+e.run_id.slice(0,8):``)}>
                            <span class="tl-time">${zo(e.timestamp)}</span>
                            <span class="tl-icon tl-icon--${e.event_type}">${Fo[e.event_type]||`·`}</span>
                            <span class="tl-session-badge tl-session-badge--${e.session_type||`chat`}">
                                ${Lo[e.session_type]||e.session_type||`chat`}
                            </span>
                            <span class="tl-event-label">${Io[e.event_type]||e.event_type}</span>
                            <span class="tl-ago">${Ro(e.timestamp)}</span>
                        </div>
                        ${e.summary&&f`
                            <div class="tl-summary${i?` tl-summary--indent`:``}"
                                 onClick=${()=>Rn(e.session_id)}>
                                ${e.summary}
                            </div>
                        `}
                    `})}
            </div>
            ${i.value&&f`
                <button class="tl-load-more"
                        onClick=${()=>c(!0)}
                        disabled=${n.value}>
                    ${n.value?`Loading...`:`Load more`}
                </button>
            `}
        </div>
    `}function Ho({tab:e}){return e===`agents`?f`<${Ha} />`:e===`workspace`?f`<${Xa} />`:e===`runs`?f`<${Mo} />`:e===`jobs`?f`<${bo} />`:e===`audit`?f`<${wo} />`:e===`timeline`?f`<${Vo} />`:null}function Uo(){X.value=null}function Wo(){return X.value?f`
        <div id="panel" class="open">
            <div class="panel-header">
                <span class="panel-header-title">${Z.value.charAt(0).toUpperCase()+Z.value.slice(1)}</span>
                <button class="panel-close-btn" title="Close panel" aria-label="Close panel"
                        onClick=${Uo}>\u00D7</button>
            </div>
            <div class="panel-body">
                <${Ho} tab=${Z.value} />
            </div>
        </div>
    `:null}var Go=()=>D(`/auth/keys`),Ko=(e,t)=>b(`/auth/keys`,{provider:e,key:t}),qo=e=>p(`/auth/keys/${e}`),Jo=[`openai`,`anthropic`,`openrouter`,`gemini`];function Yo({title:e,defaultOpen:t=!1,children:n}){let r=o(t);return f`
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
    `}function Xo({label:e,value:t,desc:n}){return f`
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
    `}function Zo(){let e=o([]),t=o(null),n=o(``),r=o(!1),i=o(``),s=async()=>{try{let t=await Go();e.value=t.keys||[]}catch(e){console.error(`[auth] list keys failed:`,e)}};a(()=>{s()},[]);let c=async e=>{if(n.value.trim()){r.value=!0,i.value=``;try{await Ko(e,n.value.trim()),n.value=``,t.value=null,await s()}catch(e){i.value=e.error?.message||e.message||`Failed to save key`}finally{r.value=!1}}},l=async e=>{try{await qo(e),await s()}catch(e){i.value=e.error?.message||e.message||`Failed to remove key`}};return f`
        <div class="settings-row">
            <label class="settings-label">API Keys</label>
            ${Jo.map(i=>{let a=e.value.find(e=>e.provider===i),o=a?.configured,s=a?.source||`none`,u=a?.key||``;return t.value===i?f`
                        <div class="api-key-row" key=${i}>
                            <span class="api-key-provider">${i}</span>
                            <input class="settings-input" type="password"
                                   autocomplete="off"
                                   placeholder="Paste API key..."
                                   value=${n.value}
                                   onInput=${e=>{n.value=e.target.value}}
                                   onKeyDown=${e=>{e.key===`Enter`&&c(i)}} />
                            <div class="api-key-actions">
                                <button class="api-key-btn save" onClick=${()=>c(i)}
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
                        <span class="api-key-value ${o?`set`:`unset`}">
                            ${o?u:`not configured`}
                        </span>
                        ${o&&s===`secrets`&&f`
                            <span class="api-key-source">stored</span>
                        `}
                        <div class="api-key-actions">
                            <button class="api-key-btn" onClick=${()=>{t.value=i,n.value=``}}>
                                ${o?`Change`:`Set`}
                            </button>
                            ${o&&s===`secrets`&&f`
                                <button class="api-key-btn remove" onClick=${()=>l(i)}>Remove</button>
                            `}
                        </div>
                    </div>
                `})}
            ${i.value&&f`<div class="inline-error">${i.value}</div>`}
        </div>
    `}function Qo({open:e,onClose:t}){let n=o(!1),r=o(``),i=o(``),s=o(``),c=o(``),l=o(``),u=o(``),d=o(``),p=o(``),m=o(``),h=o(!0),g=o(``),_=o(``),v=o(``),y=o(``),b=o(``),x=o(``),S=o(``),C=o(``),w=o(!0),ee=o(!1),T=o(``),E=o(``),te=o(!0),ne=o(!1),re=o(``),ie=o(!1),D=o(!1),O=o(!1),k=o(``);if(a(()=>{if(e){let e=W.value,t=e.context||{},a=e.session||{},o=e.tools||{},f=e.llm||{},O=f.anthropic||{},A=f.openai||{},j=f.gemini||{};r.value=t.strategy||`truncate`,i.value=t.max_input_tokens==null?``:String(t.max_input_tokens),s.value=t.compact_trigger_pct==null?``:String(t.compact_trigger_pct),c.value=t.compact_retain_pct==null?``:String(t.compact_retain_pct),l.value=t.summary_model||``,u.value=t.summary_provider||``,d.value=a.max_messages==null?``:String(a.max_messages),p.value=a.max_context_tokens==null?``:String(a.max_context_tokens),m.value=a.idle_timeout_secs==null?``:String(a.idle_timeout_secs),h.value=a.auto_archive==null||a.auto_archive,g.value=a.archive_ttl_secs==null?``:String(a.archive_ttl_secs),_.value=o.shell_policy||`sandboxed`,v.value=o.sandbox_root||`.`,y.value=o.timeout_secs==null?``:String(o.timeout_secs),b.value=o.max_output_bytes==null?``:String(o.max_output_bytes),x.value=e.model||``,S.value=e.provider||``,C.value=O.thinking_budget_tokens==null?``:String(O.thinking_budget_tokens),w.value=O.prompt_cache_enabled==null||!!O.prompt_cache_enabled,ee.value=!1,T.value=A.reasoning_effort||``,E.value=j.thinking_budget==null?``:String(j.thinking_budget),te.value=j.cache_enabled==null||!!j.cache_enabled,ne.value=!1,re.value=j.cache_ttl_seconds==null?``:String(j.cache_ttl_seconds);let M=V.value.find(e=>e.id===B.value);ie.value=!!(M&&M.debug_mode),D.value=!1,n.value=!1,k.value=``}},[e]),!e)return null;let A=W.value,j=A.context||{},M=A.session||{},N=A.logging||{},ae=A.tools||{},P=A.llm||{},oe=P.anthropic||{},se=P.openai||{},ce=P.gemini||{},le=async()=>{O.value=!0,k.value=``,n.value=!1;let e={},a={};r.value&&r.value!==(j.strategy||``)&&(a.strategy=r.value);let o=parseInt(i.value,10);!isNaN(o)&&o!==j.max_input_tokens&&(a.max_input_tokens=o);let f=parseFloat(s.value);!isNaN(f)&&f!==j.compact_trigger_pct&&(a.compact_trigger_pct=f);let N=parseFloat(c.value);!isNaN(N)&&N!==j.compact_retain_pct&&(a.compact_retain_pct=N),l.value!==(j.summary_model||``)&&(a.summary_model=l.value),u.value!==(j.summary_provider||``)&&(a.summary_provider=u.value),Object.keys(a).length>0&&(e.context=a);let P={},le=parseInt(d.value,10);!isNaN(le)&&le!==M.max_messages&&(P.max_messages=le);let F=parseInt(p.value,10);!isNaN(F)&&F!==M.max_context_tokens&&(P.max_context_tokens=F);let I=parseInt(m.value,10);!isNaN(I)&&I!==M.idle_timeout_secs&&(P.idle_timeout_secs=I),h.value!==M.auto_archive&&(P.auto_archive=h.value);let ue=parseInt(g.value,10);!isNaN(ue)&&ue!==M.archive_ttl_secs&&(P.archive_ttl_secs=ue),Object.keys(P).length>0&&(e.session=P);let L={};_.value&&_.value!==(ae.shell_policy||``)&&(L.shell_policy=_.value),v.value!==(ae.sandbox_root||``)&&(L.sandbox_root=v.value);let de=parseInt(y.value,10);!isNaN(de)&&de!==ae.timeout_secs&&(L.timeout_secs=de);let fe=parseInt(b.value,10);!isNaN(fe)&&fe!==ae.max_output_bytes&&(L.max_output_bytes=fe),Object.keys(L).length>0&&(e.tools=L);let pe={},R={},me=parseInt(C.value,10);C.value!==``&&!isNaN(me)&&me!==oe.thinking_budget_tokens&&(R.thinking_budget_tokens=me),ee.value&&w.value!==!!oe.prompt_cache_enabled&&(R.prompt_cache_enabled=w.value),Object.keys(R).length>0&&(pe.anthropic=R);let he={},ge=se.reasoning_effort||``;T.value!==ge&&(he.reasoning_effort=T.value),Object.keys(he).length>0&&(pe.openai=he);let _e={},ve=parseInt(E.value,10);E.value!==``&&!isNaN(ve)&&ve!==ce.thinking_budget&&(_e.thinking_budget=ve),ne.value&&te.value!==!!ce.cache_enabled&&(_e.cache_enabled=te.value);let ye=parseInt(re.value,10);if(re.value!==``&&!isNaN(ye)&&ye!==ce.cache_ttl_seconds&&(_e.cache_ttl_seconds=ye),Object.keys(_e).length>0&&(pe.gemini=_e),Object.keys(pe).length>0&&(e.llm=pe),x.value&&x.value!==(A.model||``)&&(e.model=x.value),S.value&&S.value!==(A.provider||``)&&(e.provider=S.value),Object.keys(e).length>0)try{await ht(e),await gt()}catch(e){let t=Array.isArray(e.errors)?e.errors.join(`; `):null;k.value=t||e.message||`Failed to save server settings`}if(D.value&&B.value){let e=V.value.find(e=>e.id===B.value),t=Aa(e&&e.debug_mode,ie.value);if(Object.keys(t).length>0)try{await ha(B.value,t);let e=await pa();e&&Array.isArray(e.agents)&&We(e.agents)}catch(e){k.value=e.error?.message||e.message||`Failed to save debug mode`}}k.value||(n.value=!0),O.value=!1,k.value||setTimeout(()=>t(),600)},F=e=>{e.target===e.currentTarget&&t()},I=ae.enabled||A.enabled_tools||[];return f`
        <div class="settings-overlay open" onClick=${F}>
            <div class="settings-modal">
                <h2>Settings</h2>
                <!-- Propagation caveats stated once here rather than repeated
                     per section: a server default that runs read takes effect on
                     the next run, except a Telegram-triggered one, which reads the
                     Gateway's alms.toml + env config and never this modal's values,
                     even after a restart. A new or changed API key is the exception:
                     the secrets store is shared, so it reaches Telegram too
                     (docs/api.md § 10.2). -->
                <p class="settings-subtitle">
                    Applies from the next run. Telegram-triggered runs pick up a new or changed API key but none of the server defaults, not even after a restart: they use alms.toml and the environment.
                    Full reference: <code>docs/config.md</code>.
                </p>

                <!-- Security: API Keys -->
                <${Zo} />

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
                <${Yo} key="debug" title="Debug" defaultOpen=${!1}>
                    <${$} label="Debug mode (active agent)"
                        desc=${ja+` Applies to the selected agent only — same field as in the Agents panel.`}>
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${ie.value}
                                   disabled=${!B.value}
                                   onChange=${e=>{ie.value=e.target.checked,D.value=!0}} />
                            <span>${ie.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                <//>

                <!-- Default LLM (server-level, editable, live).
                     Pre-PR-941 this row was a disabled-display of the per-run
                     model picker; PR-941 removed that path entirely, which
                     left no UI surface for the actual server-default model.
                     The section was restored as a persistence-only knob and
                     made live in #1148: the backend now rebuilds the shared
                     LlmClient the run path reads, so the next run picks the
                     pair up with no daemon restart. Same 600ms Saved! flash
                     as every other section. -->
                <${Yo} key="defaults" title="Default LLM (model / provider)" defaultOpen=${!0}>
                    <span class="settings-hint settings-section-desc">
                        Used by agents that don't set their own model or provider in the Agents panel.
                    </span>
                    <${$} label="Default LLM model"
                        desc="Any model id the provider accepts, e.g. claude-sonnet-4-6.">
                        <input class="settings-input settings-input-sm" type="text"
                               list="model-suggestions"
                               placeholder=${A.model||`model id`}
                               value=${x.value}
                               onInput=${e=>{x.value=e.target.value}} />
                        <span class="settings-effective">
                            <${Ta} value=${x.value.trim()} defaultValue=${A.model} />
                        </span>
                    <//>
                    <${$} label="Default LLM provider"
                        desc="Needs a working API key — see API Keys above.">
                        <select class="settings-select settings-input-sm"
                                value=${S.value}
                                onChange=${e=>{S.value=e.target.value}}>
                            ${(A.llm_providers&&A.llm_providers.length>0?A.llm_providers:Jo).map(e=>{let t=Ca(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                    <datalist id="model-suggestions">
                        ${ya.map(e=>f`<option value=${e} key=${e}></option>`)}
                    </datalist>
                <//>

                <!-- Context (server-level, editable) -->
                <${Yo} key="ctx" title="Context" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        How conversation history is fitted into the model's context window.
                    </span>
                    <!-- No hint on Strategy: the two option labels below already
                         say what each strategy does, and a hint here only restated
                         them a third time. -->
                    <${$} label="Strategy">
                        <select class="settings-select settings-input-sm"
                                value=${r.value}
                                onChange=${e=>{r.value=e.target.value}}>
                            <option value="truncate">truncate — drop oldest messages to fit budget</option>
                            <option value="compact">compact — summarise old + keep recent verbatim</option>
                        </select>
                    <//>
                    <${$} label="Max input tokens"
                        desc="Token budget per request. Set it to your model's context window.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${i.value}
                               onInput=${e=>{i.value=e.target.value}} />
                    <//>
                    ${r.value===`compact`?f`
                    <${$} label="Compact trigger %"
                        desc="Compact once history fills this share of the budget. 0.50–0.95.">
                        <input class="settings-input settings-input-sm" type="number"
                               min="0.50" max="0.95" step="0.05"
                               value=${s.value}
                               onInput=${e=>{s.value=e.target.value}} />
                    <//>
                    <${$} label="Compact retain %"
                        desc="Recent history kept verbatim afterwards. 0.20–0.60, and at least 0.10 below the trigger.">
                        <input class="settings-input settings-input-sm" type="number"
                               min="0.20" max="0.60" step="0.05"
                               value=${c.value}
                               onInput=${e=>{c.value=e.target.value}} />
                    <//>
                    `:null}
                <//>

                <!-- Summary (server-level, editable) — controls BOTH the
                     in-loop compact-strategy compaction AND the post-run
                     episodic memory generation. Lifted out of the Context
                     section to make the dual-path scope obvious. -->
                <${Yo} key="summary" title="Summary" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        ${Na} Agents can override both in the Agents panel.
                    </span>
                    <${$} label="Summary model">
                        <input class="settings-input settings-input-sm" type="text"
                               placeholder="use the agent's own LLM"
                               list="model-suggestions"
                               value=${l.value}
                               onInput=${e=>{l.value=e.target.value}} />
                        <span class="settings-effective">
                            <${Ta} value=${l.value.trim()} defaultValue=${A.model} />
                        </span>
                    <//>
                    <${$} label="Summary provider">
                        <select class="settings-select settings-input-sm"
                                value=${u.value}
                                onChange=${e=>{u.value=e.target.value}}>
                            <option value="">Unset (use the agent's own LLM)</option>
                            ${(A.llm_providers&&A.llm_providers.length>0?A.llm_providers:Jo).map(e=>{let t=Ca(e);return f`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                <//>

                <!-- Session (server-level, editable) -->
                <${Yo} key="sess" title="Session" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Not enforced yet: nothing reads these values, so no session is capped, archived or deleted because of them.
                    </span>
                    <${$} label="Max messages">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${d.value}
                               onInput=${e=>{d.value=e.target.value}} />
                    <//>
                    <${$} label="Max context tokens"
                        desc="Must be at least the Context max input tokens above.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${p.value}
                               onInput=${e=>{p.value=e.target.value}} />
                    <//>
                    <${$} label="Idle timeout (seconds)">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${m.value}
                               onInput=${e=>{m.value=e.target.value}} />
                    <//>
                    <${$} label="Auto archive">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${h.value}
                                   onChange=${e=>{h.value=e.target.checked}} />
                            <span>${h.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                    <${$} label="Archive TTL (seconds)">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${g.value}
                               onInput=${e=>{g.value=e.target.value}} />
                    <//>
                <//>

                <!-- Tools (server-level, editable) -->
                <${Yo} key="tools" title="Tools" defaultOpen=${!1}>
                    <${$} label="Shell policy"
                        desc="sandboxed keeps the shell in the project root and, on Linux, under Landlock; unrestricted removes both, for every agent.">
                        <select class="settings-select settings-input-sm"
                                value=${_.value}
                                onChange=${e=>{_.value=e.target.value}}>
                            <option value="sandboxed">sandboxed</option>
                            <option value="unrestricted">unrestricted</option>
                        </select>
                    <//>
                    <${$} label="Sandbox root"
                        desc="Gateway runs are rooted at the project root, not here. A path that doesn't exist makes runs fail.">
                        <input class="settings-input settings-input-sm" type="text"
                               value=${v.value}
                               onInput=${e=>{v.value=e.target.value}} />
                    <//>
                    <!-- #112: this knob is validated and PATCHable but no tool
                         execution path reads it. Silence here would hide that a
                         value the operator sets does nothing. What *does* bound
                         each tool varies per tool and is documented in config.md;
                         this row only needs to say the knob is inert. -->
                    <${$} label="Tool timeout (seconds)"
                        desc="Not enforced yet — setting it has no effect.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${y.value}
                               onInput=${e=>{y.value=e.target.value}} />
                    <//>
                    <${$} label="Max output (bytes)"
                        desc="Not enforced yet — setting it has no effect.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${b.value}
                               onInput=${e=>{b.value=e.target.value}} />
                    <//>
                    <${Xo} label="Enabled tools" value=${`${I.length} tools`}
                        desc=${I.join(`, `)} />
                <//>

                <!-- LLM Providers (server-level, editable) — #809 / #804 Slice A -->
                <${Yo} key="llm" title="LLM Providers" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Reasoning and caching defaults. ${Ma(`block`)}
                    </span>

                    <h4 class="settings-llm-subhead">Anthropic</h4>
                    <${$} label="Thinking budget tokens"
                        desc="Extended-thinking budget. 0 turns it off; blank keeps the current value.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${oe.thinking_budget_tokens==null?`unset`:String(oe.thinking_budget_tokens)}
                               value=${C.value}
                               onInput=${e=>{C.value=e.target.value}} />
                    <//>
                    <${$} label="Prompt cache enabled"
                        desc="Caches the stable request prefix for 5 minutes.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${w.value}
                                   onChange=${e=>{w.value=e.target.checked,ee.value=!0}} />
                            <span>${w.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>

                    <h4 class="settings-llm-subhead">OpenAI / OpenRouter</h4>
                    <${$} label="Reasoning effort"
                        desc="Reasoning models only (o-series, GPT-5, Grok); dropped on any other model.">
                        <select class="settings-select settings-input-sm"
                                value=${T.value}
                                onChange=${e=>{T.value=e.target.value}}>
                            <option value="">Unset (no override)</option>
                            <option value="minimal">minimal</option>
                            <option value="low">low</option>
                            <option value="medium">medium</option>
                            <option value="high">high</option>
                        </select>
                    <//>

                    <h4 class="settings-llm-subhead">Gemini</h4>
                    <${$} label="Thinking budget"
                        desc="Extended-thinking budget. 0 turns it off; blank keeps the current value.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${ce.thinking_budget==null?`unset`:String(ce.thinking_budget)}
                               value=${E.value}
                               onInput=${e=>{E.value=e.target.value}} />
                    <//>
                    <${$} label="Cache enabled"
                        desc="Gemini context caching. Needs a 32k-token prefix; below that Gemini ignores it.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${te.value}
                                   onChange=${e=>{te.value=e.target.checked,ne.value=!0}} />
                            <span>${te.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                    <${$} label="Cache TTL (seconds)"
                        desc="Must be greater than 0.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="60"
                               placeholder=${ce.cache_ttl_seconds==null?`300`:String(ce.cache_ttl_seconds)}
                               value=${re.value}
                               onInput=${e=>{re.value=e.target.value}} />
                    <//>
                <//>

                <!-- Logging (server-level, read-only) -->
                <${Yo} key="log" title="Logging" defaultOpen=${!1}>
                    <!-- Read-only rows: label + value is the whole story, so the
                         per-row descriptions that only restated the label are gone. -->
                    <span class="settings-hint settings-section-desc">
                        Read-only — edit <code>alms.toml</code> and restart to change.
                    </span>
                    <${Xo} label="File logging" value=${N.file_enabled==null?`--`:N.file_enabled?`enabled`:`disabled`} />
                    <${Xo} label="File level" value=${N.file_level||`--`} />
                    <${Xo} label="Rotation" value=${N.rotation||`--`} />
                    <${Xo} label="Log directory" value=${N.log_dir||`default (data/logs/)`} />
                <//>

                <div class="settings-divider"></div>

                <!-- Server info (compact) -->
                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Version: <span class="settings-info-value">${A.version||`unknown`}</span></div>
                        <div>Base URL: <span class="settings-info-value">${A.base_url||`unknown`}</span></div>
                        <div>Stream timeout: <span class="settings-info-value">${A.stream_chunk_timeout_secs||180}s</span></div>
                    </div>
                </div>

                ${k.value&&f`
                    <div class="settings-error">
                        Failed to save server settings: ${k.value}
                    </div>
                `}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${t}>Cancel</button>
                    <button class="settings-save" onClick=${le}
                            disabled=${O.value}>
                        ${O.value?`Saving...`:n.value?`Saved!`:`Apply`}
                    </button>
                </div>
            </div>
        </div>
    `}var $o=`openrouter`;function es(e){let t=e&&e.keys;return Array.isArray(t)?t.some(e=>e&&e.configured===!0&&Jo.includes(e.provider)):!1}var ts=`https://openrouter.ai/keys`;function ns({onSaved:e,onSkip:t}){let n=o(``),r=o(``),i=o(!1),s=l(null);return a(()=>{s.current?.focus()},[]),f`
        <form class="onboard-card" onSubmit=${async t=>{t.preventDefault();let a=n.value.trim();if(a){i.value=!0,r.value=``;try{await Ko($o,a),n.value=``,e()}catch(e){r.value=e.error?.message||e.message||`Failed to save key`}finally{i.value=!1}}}}>
            <div class="onboard-step">Step 1 of 2</div>
            <h2>Welcome to ALMS</h2>
            <p>
                An agent answers through an LLM provider, so it needs a key before it can reply
                at all. <strong>OpenRouter</strong> is the recommended one: it is the default
                provider, and one key there covers both defaults —${` `}
                <code>z-ai/glm-5.2</code> for chat and <code>google/gemma-4-31b-it</code> for
                summaries (compaction and episodic memory). Nothing else to configure.
            </p>
            <div>
                <div class="onboard-label-row">
                    <label>OpenRouter API key</label>
                    <a href=${ts} target="_blank" rel="noopener noreferrer">Get a key</a>
                </div>
                <input type="password" autocomplete="off" placeholder="sk-or-..."
                    value=${n.value}
                    ref=${s}
                    onInput=${e=>{n.value=e.target.value}}
                    disabled=${i.value} />
                <div class="onboard-hint">
                    Applies to the running gateway immediately — no restart needed.
                </div>
            </div>
            <button class="onboard-btn" type="submit" disabled=${i.value||!n.value.trim()}>
                ${i.value?`Saving...`:`Save Key`}
            </button>
            <div>
                <button class="onboard-skip" type="button" onClick=${t} disabled=${i.value}>
                    Skip for now
                </button>
                <div class="onboard-hint">
                    A key declared in <code>alms.toml</code>${` `}
                    (<code>api_key_env</code> under <code>[llm.providers.openrouter]</code>)
                    works but is not visible from here. You can also set one later in
                    Settings.
                </div>
            </div>
            <div class="onboard-error">${r.value}</div>
            <p class="onboard-footnote">
                The provider, the chat model and the summary model can all be changed later in
                Settings (the gear in the header).
            </p>
        </form>
    `}function rs({stepLabel:e,keySaved:t}){let n=o(``),r=o(``),i=o(!1),s=l(null);return a(()=>{s.current?.focus()},[]),f`
        <form class="onboard-card" onSubmit=${async e=>{e.preventDefault();let t=n.value.trim();if(t){if(!/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,62}[A-Za-z0-9])?$/.test(t))r.value=`Invalid name: letters, digits, hyphens only (1-64 chars, no trailing hyphen)`;else{i.value=!0,r.value=``;try{let e=await ma({name:t,is_default:!0}),n=await pa();We(n.agents||[]);let r=e.id||(V.value.find(e=>e.name===t)||{}).id;r?await gn(r):console.warn(`[onboarding] POST /agents returned no id for agent:`,t,e)}catch(e){r.value=e.error?.message||e.message||`Failed to create agent`}finally{i.value=!1}}}}}>
            ${e&&f`<div class="onboard-step">${e}</div>`}
            <h2>Name your agent</h2>
            ${t&&f`<div class="onboard-note">OpenRouter key saved — live now, no restart.</div>`}
            <p>Create your first agent to get started. The agent will introduce itself and learn about you in a short setup conversation.</p>
            <div>
                <label>Agent name</label>
                <input type="text" placeholder="my-agent"
                    value=${n.value}
                    ref=${s}
                    onInput=${e=>{n.value=e.target.value}}
                    disabled=${i.value} />
                <div class="onboard-hint">letters, digits, hyphens (1-64 chars)</div>
            </div>
            <button class="onboard-btn" type="submit" disabled=${i.value||!n.value.trim()}>
                ${i.value?`Creating...`:`Create Agent`}
            </button>
            <div class="onboard-error">${r.value}</div>
        </form>
    `}function is(){let e=o(`probing`),t=o(!1),n=o(!1);return a(()=>{let n=!1,r=r=>{n||(e.value=r,r===`key`&&(t.value=!0))};return Go().then(e=>r(es(e)?`name`:`key`)).catch(e=>{console.warn(`[onboarding] GET /auth/keys failed:`,e),r(`key`)}),()=>{n=!0}},[]),e.value===`probing`?f`
            <div id="onboarding">
                <div class="onboard-card">
                    <h2>Welcome to ALMS</h2>
                    <p class="onboard-hint">Checking configuration...</p>
                </div>
            </div>
        `:f`
        <div id="onboarding">
            ${e.value===`key`?f`<${ns}
                    onSaved=${()=>{n.value=!0,e.value=`name`}}
                    onSkip=${()=>{e.value=`name`}} />`:f`<${rs}
                    stepLabel=${t.value?`Step 2 of 2`:null}
                    keySaved=${n.value} />`}
        </div>
    `}function as(e){if(!e)return``;if(e.status===`done`)return`Done`;if(e.status===`fail`)return`Failed`;if(e.status===`cancelled`)return`Cancelled`;let t=e.activity;if(!t||!t.kind)return`Starting…`;switch(t.kind){case`reasoning`:return`Reasoning…`;case`writing`:return`Writing…`;case`tool_start`:return t.tool?`Using ${t.tool}`:`Using tool`;case`tool_end`:return`Running…`;default:return`Running…`}}function os(){let e=Object.entries(Ue.value);return e.length===0?null:f`
        <div class="sa-bar" aria-label="Subagent status bar">
            ${e.map(([e,t])=>{let n=t.status===`running`,r=t.status===`done`?`✓`:`✗`,i=t.displayName||e,a=as(t),o=()=>{t.sessionId&&ve(t.sessionId)},s=e=>{ka(e)&&o()},c=e=>{Oa(e)&&(e.preventDefault(),o())},l=t.task?`${i}: ${t.task} — open subagent session`:`${i} — open subagent session`,u=de(t.sessionId),d=e=>t=>{t.stopPropagation(),t.key===`Escape`?(t.preventDefault(),fe()):(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),e(t))},p=e=>{e.stopPropagation(),ge(t.sessionId)},m=e=>{e.stopPropagation(),ye(t.sessionId)},h=e=>{e.stopPropagation(),fe()};return f`
                    <div class="sa-chip ${n?`running`:t.status}"
                         role="button"
                         tabindex="0"
                         title=${l}
                         onClick=${s}
                         onKeyDown=${c}>
                        ${n?f`<span class="tc-spinner"></span>`:f`<span>${r}</span>`}
                        <span class="sa-chip-name">${i}</span>
                        ${a&&f`<span class="sa-chip-status">${a}</span>`}
                        ${Ae(t)&&(u?f`
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
    `}function ss(){let e=H.value,{phase:t,detail:n}=Fe.value,r=Ee(t,n),i=o(!1),s=o(!1),c=l(null),d=u(()=>{i.value=!i.value},[]),p=u(()=>{i.value=!1,s.value=!0},[]);return a(()=>{if(!i.value)return;let e=e=>{c.current&&!c.current.contains(e.target)&&(i.value=!1)};return document.addEventListener(`click`,e,!0),()=>document.removeEventListener(`click`,e,!0)},[i.value]),e?f`
        <div class="agent-header-bar">
            <div class="agent-header-bar-left">
                <span class="agent-header-bar-name">${e.name}</span>
                ${r&&f`
                    <span class="agent-status-label">${r}</span>
                `}
            </div>
            <div class="agent-header-bar-right">
                <button class="hbtn agent-bar-btn ${X.value===`workspace`?`active`:``}"
                        title="Workspace files"
                        aria-label="Open workspace panel"
                        onClick=${()=>_n(`workspace`)}>
                    <${Cn} />
                    <span class="agent-bar-btn-label">Workspace</span>
                </button>
                <button class="hbtn agent-bar-btn ${X.value===`timeline`?`active`:``}"
                        title="Agent timeline"
                        aria-label="Open timeline panel"
                        onClick=${()=>_n(`timeline`)}>
                    <${Sn} />
                    <span class="agent-bar-btn-label">Timeline</span>
                </button>
                <button class="hbtn agent-bar-btn ${X.value===`runs`?`active`:``}"
                        title="Agent runs"
                        aria-label="Open runs panel"
                        onClick=${()=>_n(`runs`)}>
                    <${Dn} />
                    <span class="agent-bar-btn-label">Runs</span>
                </button>
                <div class="agent-menu-anchor" ref=${c}>
                    <button class="hbtn agent-bar-btn"
                            title="Agent menu"
                            aria-label="Open agent menu"
                            aria-expanded=${i.value}
                            onClick=${d}>
                        <span class="agent-menu-dots" aria-hidden="true">\u22EF</span>
                    </button>
                    ${i.value&&f`
                        <div class="agent-menu-dropdown">
                            <button class="agent-menu-item" onClick=${p}>
                                Settings
                            </button>
                        </div>
                    `}
                </div>
            </div>

            ${s.value&&f`
                <${Ba}
                    agent=${e}
                    onClose=${()=>{s.value=!1}} />
            `}
        </div>
    `:null}var cs=s(!1),ls=new Set;function us(e,t,n){return e.fromAgent?e.fromAgent===t[0]?`left`:`right`:e.type===`agent`||e.role===`assistant`?n?n===t[0]?`left`:`right`:`left`:e.type===`user`||e.role===`user`?n?n===t[0]?`right`:`left`:`right`:`center`}function ds({msg:e,participants:t,perspectiveAgent:n}){let r=us(e,t,n),i=e.fromAgent||(r===`left`?t[0]:t[1])||`?`,o=d(e.text||``),s=e.type===`agent`||e.role===`assistant`,c=l(null);return a(()=>{s&&Br(c.current)},[o,s]),f`
        <div class="dm-msg dm-msg-${r}">
            <div class="dm-msg-name-row dm-msg-name-row-${r}">
                <div class="dm-msg-name">${i}</div>
                <${Vr} ts=${e.ts} />
            </div>
            <div class="dm-msg-bubble markdown-body" ref=${c}
                 dangerouslySetInnerHTML=${{__html:o}} />
        </div>
    `}function fs({text:e}){return f`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">${e}</span>
        </div>
    `}function ps(e,t){if(!e)return!1;let n=e.trim();if(!n)return!1;for(let e of t||[]){if(e.tool!==`send_message`)continue;let t=e.params&&typeof e.params.message==`string`?e.params.message.trim():``;if(t&&t===n)return!0}return!1}function ms({runId:e,agentName:t,thinkingText:r,tools:i,status:a,isLive:o}){let[s,c]=n(!1),l=o&&Ne.value.get(e)||``,u=r||l,d=ps(u,i)?``:u,p=(i||[]).filter(e=>e.tool!==`send_message`||e.status!==`done`),m=p.length,h=(i||[]).length>0;if(!o&&!h&&(!d||!d.trim()))return null;let g=`dm-reasoning-block`+(a===`failed`?` dm-reasoning-block--failed`:``)+(o?` dm-reasoning-block--live`:``),_=s?`▼`:`▶`,v=t?`${t} reasoning -- ${m} tool call${m===1?``:`s`}`:`Agent reasoning -- ${m} tool call${m===1?``:`s`}`;return f`
        <div class=${g}>
            <div class="dm-reasoning-header" onClick=${()=>c(!s)}>
                <span class="dm-reasoning-toggle">${_}</span>
                <span class="dm-reasoning-summary">${v}</span>
                ${o&&f`<span class="dm-reasoning-spinner" />`}
            </div>
            ${s&&f`
                <div class="dm-reasoning-body">
                    ${d&&d.trim()&&f`
                        <pre class="dm-reasoning-thinking">${d}</pre>
                    `}
                    ${p.map(e=>f`
                        <${Ii} key=${e.id} ...${e} />
                    `)}
                </div>
            `}
        </div>
    `}async function hs(){let e=C.value;if(e&&!cs.value){cs.value=!0;try{await ue(e)}catch(e){console.error(`[cancel-dm] failed:`,e)}finally{cs.value=!1}}}function gs(){let e=l(null),t=ee.value;a(()=>{let t=0,n=i(()=>{M.value,cancelAnimationFrame(t),t=requestAnimationFrame(()=>{Tr(e.current)})});return()=>{cancelAnimationFrame(t),n()}},[]);let n=M.value,r=H.value?H.value.name:null,o=t.length>=2?`${t[0]} <-> ${t[1]}`:`DM conversation`,s=!!U.value,c=!!De.value,u=s||c,d=cs.value;return f`
        <div class="dm-view-header">
            <span class="dm-view-header-icon" aria-hidden="true">\u2194</span>
            <span class="dm-view-header-label">${o}</span>
            <span class="dm-view-header-badge">read-only</span>
        </div>
        <div class="dm-thread" ref=${e}>
            ${n.length===0&&f`
                <div class="empty-state">No messages in this conversation yet.</div>
            `}
            ${n.map(e=>{if(e.type===`dm_ended`){let t=`Conversation ended -- ${e.reason||`ended`}`;return f`<${fs} key=${e.id} text=${t} />`}if(e.type===`system`)return f`<${fs} key=${e.id} text=${e.text} />`;if(e.type===`notification`){let t=e.metadata||{};if(t.type===`dm_ended_notification`){let n=we[t.reason]||t.reason||`ended`,r=t.detail?`DM with ${t.peer||`unknown`} ended -- ${n}: ${t.detail}`:`DM with ${t.peer||`unknown`} ended -- ${n}`;return f`<${fs} key=${e.id} text=${r} />`}return f`<${fs} key=${e.id} text=${e.text} />`}if(e.type===`error`)return f`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-error">${e.text}</div></div>`;if(e.type===`tokens`)return null;if(e.type===`thinking`){let t=`Thinking…`;if(e.pending)t=`Sending…`;else if(e.queuedBehind>0)t=`Queued \u2014 position ${e.queuedBehind}\u2026`;else if(e.source){let n=e.source.startsWith(`peer:`)?e.source.slice(5):e.source;n&&(t=`${n} is thinking\u2026`)}return f`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-thinking">${t}</div></div>`}if(e.type===`warning`)return f`<${fs} key=${e.id} text=${e.text||`Warning`} />`;if(e.type===`run_boundary`){if(!e.status||e.status===`completed`)return null;let t=e.status===`failed`?`run failed`:e.status===`cancelled`?`run cancelled`:`run ${e.status}`;return f`<${fs} key=${e.id} text=${t} />`}if(e.type===`subagent_completed`){let t=`Subagent '${e.name||`subagent`}' ${e.status===`fail`?`failed`:`completed`}`;return f`<${fs} key=${e.id} text=${t} />`}if(e.type===`job_completed`)return f`<${fs} key=${e.id} text=${`Job '${e.jobName||`job`}' ${e.status||`completed`}`} />`;if(e.type===`context_debug`)return f`<${Hi} key=${e.id} ...${e} />`;if(e.type===`dm_reasoning`)return f`<${ms} key=${e.id} ...${e} />`;if(e.type===`tool`){if(e.tool===`send_message`&&e.status===`done`&&!e.error)return null;ls.has(e.id)||(ls.add(e.id),console.warn(`[DmConversationView] ungrouped DM tool rendered as a standalone sibling row — this fallback is meant to be dead post-#1076/#1154. Tool:`,e.tool,`id:`,e.id,`runId:`,e.runId));let n=us({type:`agent`,role:`assistant`},t,r),i=n===`left`?t[0]:t[1];return f`
                        <div key=${e.id} class="dm-msg dm-msg-${n} dm-msg-tool-row">
                            <div class="dm-msg-name">${i||`?`}</div>
                            <${Ii} ...${e} />
                        </div>
                    `}if(e.type===`image`){let n=us(e,t,r),i=e.fromAgent||(n===`left`?t[0]:t[1])||`?`;return f`
                        <div key=${e.id} class="dm-msg dm-msg-${n}">
                            <div class="dm-msg-name-row dm-msg-name-row-${n}">
                                <div class="dm-msg-name">${i}</div>
                                <${Vr} ts=${e.ts} />
                            </div>
                            <div class="dm-msg-bubble">
                                ${e.url?f`<img src=${e.url} alt=${e.alt||``} class="dm-msg-image" />`:`[Image${e.alt?`: `+e.alt:``}]`}
                            </div>
                        </div>
                    `}return e.type===`user`||e.type===`agent`?f`<${ds} key=${e.id} msg=${e} participants=${t} perspectiveAgent=${r} />`:null})}
        </div>
        <div class="dm-view-footer">
            ${u?f`
                    <button class="dm-cancel-btn"
                            disabled=${d}
                            title="Stop this DM conversation"
                            aria-label="Stop conversation"
                            onClick=${hs}>
                        <span class="dm-cancel-btn-icon" aria-hidden="true">\u25A0</span>
                        ${d?`Stopping…`:`Stop conversation`}
                    </button>
                `:f`
                    <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
                `}
        </div>
    `}function _s(){return ze.value?f`
        <button
            type="button"
            class="stream-dead-banner"
            role="alert"
            aria-live="polite"
            onClick=${Ce}
            title="Click to reconnect live updates"
        >
            <span class="stream-dead-banner-icon" aria-hidden="true">⚠</span>
            <span class="stream-dead-banner-text">
                Live updates disconnected — click to reconnect or reload.
            </span>
        </button>
    `:null}var vs=e({status:()=>ys});i(()=>{let e=H.value;document.title=e?`ALMS - ${e.name}`:`ALMS`});var ys=s(`connecting...`);function bs(e){let t=[],n=0;for(;n<e.length;)if(e[n].type===`tool`){let r=[];for(;n<e.length&&e[n].type===`tool`;)r.push(e[n]),n++;r.length>1?t.push({_isToolGroup:!0,key:`tg-`+r[0].id,tools:r}):t.push(r[0])}else t.push(e[n]),n++;return t}function xs(){let e=l(null);a(()=>{let t=0,n=i(()=>{M.value,cancelAnimationFrame(t),t=requestAnimationFrame(()=>{Tr(e.current)})});return()=>{cancelAnimationFrame(t),n()}},[]);let t=bs(M.value),n=x.value,r=m.value,o=le.value,s=ne.value,c=ce.value,u=o?s?.agent_name?s.agent_name+` notifications`:`Notification session`:s?.session_type===`job`?c?c+` job session`:`Job session`:s?.session_type===`subagent`?`Subagent session`:`Internal session`,d=o?`⚡`:s?.session_type===`job`?`⏰`:`⚙`,p=s?.session_type?`internal-session-`+s.session_type:``;return f`
        <div id="chat">
            <${ss} />
            ${(vt.value||_t.value)&&f`
                <div id="messages" role="log" aria-live="polite">
                    ${vt.value?f`<div class="loading-state">Loading agent...</div>`:f`<div class="loading-state">Loading session...</div>`}
                </div>
            `}
            ${!vt.value&&!_t.value&&n&&f`
                <${gs} />
            `}
            ${!vt.value&&!_t.value&&!n&&f`
            ${r&&f`
                <div class="internal-session-header ${p}">
                    <span class="internal-session-header-icon" aria-hidden="true">${d}</span>
                    <span class="internal-session-header-label">${u}</span>
                    <span class="internal-session-header-badge">read-only</span>
                </div>
            `}
            ${L.value&&f`
                <div class="sa-breadcrumb">
                    <button class="sa-breadcrumb-btn" onClick=${()=>He()}>
                        \u2190 Back to parent session
                    </button>
                    ${U.value&&(de(C.value)?f`
                            <span class="sa-cancel-confirm-group sa-breadcrumb-cancel" role="group"
                                  aria-label="Confirm cancel subagent"
                                  onKeyDown=${e=>{e.key===`Escape`&&(e.preventDefault(),fe())}}>
                                <span class="sa-cancel-confirm-label">Cancel this subagent?</span>
                                <button class="sa-confirm-btn sa-confirm-yes"
                                        title="Yes, cancel this subagent"
                                        onClick=${()=>ye(C.value)}>Yes</button>
                                <button class="sa-confirm-btn sa-confirm-no"
                                        title="No, keep it running"
                                        onClick=${()=>fe()}>No</button>
                            </span>
                        `:f`
                            <button class="sa-breadcrumb-cancel-btn sa-breadcrumb-cancel"
                                    title="Cancel this subagent"
                                    onClick=${()=>ge(C.value)}>
                                Cancel subagent
                            </button>
                        `)}
                </div>
            `}
            <div id="messages" role="log" aria-live="polite" ref=${e}>
                ${M.value.length===0&&f`
                    <div class="empty-state">
                        ${r?`No activity recorded in this session yet.`:`No messages yet. Send a message to start.`}
                    </div>
                `}
                ${t.map(e=>{if(e._isToolGroup)return f`
                            <${Li} key=${e.key} count=${e.tools.length}>
                                ${e.tools.map(e=>f`<${Ii} key=${e.id} ...${e} />`)}
                            <//>
                        `;let t=e;if(t.type===`user`||t.type===`agent`)return f`<${Gr} key=${t.id} type=${t.type} text=${t.text} sealed=${t.sealed} fromAgent=${t.fromAgent} reasoning=${t.reasoning} ts=${t.ts} />`;if(t.type===`tool`)return f`<${Ii} key=${t.id} ...${t} />`;if(t.type===`context_debug`)return f`<${Hi} key=${t.id} ...${t} />`;if(t.type===`approval`)return f`<${Wi} key=${t.id} ...${t} />`;if(t.type===`job_completed`)return f`<${$i} key=${t.id} jobName=${t.jobName} status=${t.status} summary=${t.summary} ts=${t.ts} runId=${t.runId} truncated=${t.truncated} jobSessionUuid=${t.jobSessionUuid} jobSessionId=${t.jobSessionId} />`;if(t.type===`subagent_completed`)return f`<${ia} key=${t.id}
                            name=${t.name} task=${t.task} status=${t.status}
                            toolCount=${t.toolCount} durationMs=${t.durationMs}
                            sessionId=${t.sessionId} summary=${t.summary} />`;if(t.type===`image`)return f`<${Kr} key=${t.id}
                            role=${t.role} fromAgent=${t.fromAgent} ts=${t.ts}
                            url=${t.url} alt=${t.alt} />`;if(t.type===`error`)return f`<${Yr} key=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`warning`)return f`<${Xr} key=${t.id} id=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`run_boundary`)return f`<${Qr} key=${t.id} status=${t.status} error=${t.error} />`;if(t.type===`system`)return f`<${Zr} key=${t.id} text=${t.text} />`;if(t.type===`dm_ended`)return f`<${$r} key=${t.id} peer=${t.peer} reason=${t.reason} detail=${t.detail} />`;if(t.type===`notification`){let e=t.metadata||{};return e.type===`dm_ended_notification`?f`<${$r} key=${t.id} peer=${e.peer||`unknown`} reason=${we[e.reason]||e.reason||`conversation ended`} detail=${e.detail} />`:f`<${Zr} key=${t.id} text=${t.text} />`}return t.type===`tokens`?f`<${Jr} key=${t.id} usage=${t.usage} />`:t.type===`thinking`?f`<${qr} key=${t.id}
                            pending=${t.pending} queuedBehind=${t.queuedBehind} source=${t.source} />`:null})}
            </div>
            <${oa} />
            <${os} />
            ${r?f`
                    <div class="internal-session-footer">
                        <span class="internal-session-footer-text">This is a read-only view of internal agent activity.</span>
                    </div>
                `:f`<${fa} />`}
            `}
        </div>
    `}function Ss(){let e=o(!1),t=V.value.length>0;return f`
        <${Ln} status=${ys} onOpenSettings=${()=>{e.value=!0}} />
        <${_s} />
        ${t?f`
                <div id="main">
                    <${xr} />
                    <${xs} />
                    <${Wo} />
                </div>`:f`<${is} />`}
        <${Qo} open=${e.value} onClose=${()=>{e.value=!1}} />
    `}r(f`<${Ss} />`,document.getElementById(`app`));function Cs(){yt.value=!1,ys.value=`connecting...`,pn().then(()=>{ys.value=`connected`}).catch(()=>{ys.value=`offline`,yt.value=!0})}xt(Cs),Pe(),Cs();export{pn as a,gn as c,yt as d,St as f,sa as i,Yt as l,xt as m,fa as n,mn as o,_t as p,ca as r,un as s,ys as status,vs as t,vt as u};