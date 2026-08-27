import{a as e,c as t,d as n,i as r,l as i,n as a,o,r as s,s as c,u as l}from"./index-MpsRiD1K.js";import{n as u,t as d}from"./deps-DCoVQUe-.js";import{A as f,C as p,D as m,E as h,M as g,N as _,S as v,T as y,_ as b,a as x,b as S,c as C,d as w,f as T,g as E,h as D,i as ee,j as te,k as ne,l as re,m as ie,o as ae,p as O,r as k,s as A,t as j,u as M,v as N,w as oe,x as P,y as se}from"./pending-messages-gcKUMp-J.js";import{A as ce,C as F,D as le,E as I,I as L,M as ue,O as R,P as de,S as fe,T as pe,a as me,b as he,c as ge,d as _e,g as ve,h as ye,j as be,k as xe,l as Se,n as Ce,o as we,p as Te,r as Ee,s as De,t as Oe,u as ke,x as Ae,y as je}from"./use-session-stream-TOS216PK.js";import{t as z}from"./entity-state-Dvifu_4n.js";import{activeAgent as B,activeAgentId as V,agents as H,replaceAgents as Me}from"./agents-CssV4lAk.js";import{activeRunId as U,clearRuns as Ne,replaceRuns as Pe,runListGeneration as Fe,selectedRunId as Ie}from"./runs-DkBYMjgB.js";import{a as Le,i as Re,o as ze,r as Be}from"./chat-actions-uXEv39RD.js";import{agentSwitchLoading as Ve,bootRetryAvailable as He,runBoot as Ue,sessionSwitchLoading as We,setRunBoot as Ge}from"./loading-BUKrrlSY.js";import{n as Ke,t as qe}from"./select-generation-DvILpFQd.js";import{n as Je,o as Ye,r as Xe,t as Ze}from"./runs-BCRCnSa8.js";import{t as Qe}from"./load-session-B0iLD1Zb.js";var $e=()=>f(`/settings`),et=e=>te(`/settings`,e),W=o({});async function tt(){try{W.value=await $e()}catch(e){console.error(`[settings] refresh failed:`,e)}}var G=o(null),K=o(null),q=null,nt=null,rt=0,it=10,at=null,J=null,ot=null;function st(e,t){let n=t&&t.streamEpoch!=null?String(t.streamEpoch):null;if(lt(),!e)return;ot!==null&&(clearTimeout(ot),ot=null);let r=localStorage.getItem(`alms_auth_token`),i=new URLSearchParams;r&&i.set(`token`,r),t&&t.lastEventId!=null&&i.set(`last_event_id`,String(t.lastEventId)),n&&i.set(`stream_epoch`,n);let a=i.toString(),o=`/events/session-activity${a?`?`+a:``}`,s=new EventSource(o);q=s,nt=e,rt=0,at=t&&t.lastEventId!=null?t.lastEventId:null,J=n;let c=!1,l=!1,u=!1,d=null,f=null,p=0,m=!1,h=(e,t,n)=>re(e,t,n,J),g=async(e,t=J)=>{let n=Number.isSafeInteger(e)?e:null,r=t;if(l){u=!0,f===r?n!=null&&(d=d==null?n:Math.max(d,n)):(f=r,d=n);return}if(s!==q)return;l=!0;let i=M(n,r),a=null;try{a=await L(null,{includeDms:!0})}catch(e){console.error(`[agent-events] activity reconciliation failed:`,e)}if(s!==q){C(i);return}a?T(i,a.sessions||[])?p=0:(u=!0,d=n,f=r):C(i),l=!1;let o=u,c=d,m=f;if(u=!1,d=null,f=null,o&&s===q){g(c,m);return}if(!a&&s===q){p++;let e=Math.min(1e3*2**(p-1),3e4);s._reconciliationRetryTimer=setTimeout(()=>{s._reconciliationRetryTimer=null,s===q&&g(null)},e)}};s.addEventListener(`open`,()=>{if(s!==q)return;let e=c||!!(t&&t.reconcileOnOpen);c=!0,me(`agent-events`),e&&g(null)});let _=async(t,n)=>{if(!(m||s!==q)){m=!0,s.close(),console.error(`[agent-events] rejected live event; reconciling:`,n);try{let n=await L(null,{includeDms:!0});if(s!==q)return;ie(n.sessions||[],t,J),q=null,st(e,{lastEventId:t,streamEpoch:J,reconcileOnOpen:!0})}catch(e){console.error(`[agent-events] contract reconciliation failed:`,e),De(`agent-events`)}}},v=(e,t)=>s.addEventListener(e,n=>{if(s!==q)return;let r=n.lastEventId,i=r&&/^\d+$/.test(r)?Number(r):null;try{let a=globalThis.__almsContracts;if(!a)throw Error(`Frontend contract bridge is not installed`);t({data:a.parseSseJsonPayload(e,n.data),lastEventId:n.lastEventId}),i!=null&&(at=r)}catch(t){i==null?(console.error(`[agent-events]`,e,`handler failed:`,t),s.close(),De(`agent-events`)):_(i,t)}});v(`session_activity_started`,e=>{let t=e.data,n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;h(`session_activity_started`,t,n)}),v(`session_activity_ended`,e=>{let t=e.data,n=/^\d+$/.test(e.lastEventId)?Number(e.lastEventId):null;h(`session_activity_ended`,t,n)}),v(`stream_state`,e=>{let t=e.data,n=!!(J&&t.stream_epoch&&J!==t.stream_epoch);if(t.stream_epoch&&(J=t.stream_epoch),t.requires_reconciliation||n){let e=Number.isSafeInteger(t.newest)?t.newest:null;g(e)}}),s.onerror=()=>{if(s.readyState===EventSource.CLOSED){if(rt++,rt>=it){console.error(`[agent-events] Max retries reached for agent`,e),De(`agent-events`);return}let t=Math.min(2e3*2**(rt-1),3e4),n=e,r=at,i=J;ot=setTimeout(()=>{ot=null,nt===n&&st(n,{lastEventId:r,streamEpoch:i,reconcileOnOpen:!0})},t)}}}function ct(){rt=0;let e=nt;e?st(e,{lastEventId:at,streamEpoch:J,reconcileOnOpen:!0}):me(`agent-events`)}Se(ct);function lt(){q&&=(q._reconciliationRetryTimer!=null&&(clearTimeout(q._reconciliationRetryTimer),q._reconciliationRetryTimer=null),q.close(),null),nt=null,at=null,J=null,rt=0,ot!==null&&(clearTimeout(ot),ot=null),me(`agent-events`)}function ut(e,t){return!t||typeof t!=`string`?e??null:t===e?null:t}function dt(e){return!e||typeof e!=`string`?null:e}function ft(e,t){return!t||typeof t!=`string`||!e||typeof e!=`string`?!1:e===t}function pt(e){let t=new Map;if(!Array.isArray(e))return t;for(let n of e){if(!n||typeof n.agent_id!=`string`||!n.agent_id)continue;let e=t.get(n.agent_id);e?e.push(n):t.set(n.agent_id,[n])}return t}function mt(e,t){if(!e||!t||typeof t!=`string`)return!1;if(e.session_type===`notification`)return e.agent_name===t;if(e.session_type===`dm`){let n=e.participants;return Array.isArray(n)&&n.includes(t)}return!1}function ht(e,t){if(!Array.isArray(e))return[];let n=e.map((e,n)=>({s:e,idx:n,owned:+!mt(e,t)}));return n.sort((e,t)=>e.owned-t.owned||e.idx-t.idx),n.map(e=>e.s)}function gt(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type!==`notification`&&e.session_type!==`job`):[]}function _t(e){return Array.isArray(e)?e.filter(e=>e&&e.session_type===`job`):[]}var vt=`alms_active_agent`,Y=0;function yt(e){return`alms_active_session_${e}`}function bt(e,t){e&&t&&localStorage.setItem(yt(e),t)}function xt(e,t,n){if(n){let e=t.find(e=>e.id===n);if(e)return e}let r=localStorage.getItem(yt(e));if(r){let e=t.find(e=>e.id===r);if(e)return e}return t[0]||null}async function St(e,t){let n=localStorage.getItem(yt(e));if(!n||t.some(e=>e.id===n))return null;try{return await de(n),n}catch(t){return t&&t.status===404&&localStorage.removeItem(yt(e)),null}}async function Ct(){try{let e=await $e();W.value=e,Me(e.agents||[]);let t=localStorage.getItem(vt),n=H.value.find(e=>e.is_default),r=H.value[0],i=H.value.find(e=>e.id===t)||n||r;i&&(V.value=i.id,S.value=dt(i.id),localStorage.setItem(vt,i.id),await Tt(i.id))}catch(e){throw console.error(`[boot] failed:`,e),e}}async function wt(){try{return(await L(null,{includeDms:!0})).sessions||[]}catch(e){return console.error(`[fetchCrossAgentSurfaces] failed:`,e),[]}}async function Tt(e,t){let n=++Y;try{let[r,i]=await Promise.all([L(e,{includeDms:!1}),wt()]);if(n!==Y)return;let a=gt(r.sessions||[]);y(a,i),ie([...a,...i]),st(e);let o=t?null:await St(e,a);if(n!==Y)return;if(o)E.value=o,bt(e,o),await Qe(o,{isStale:()=>n!==Y,logPrefix:`loadAgentSessions:hidden`});else if(a.length>0){let r=xt(e,a,t);E.value=r.id,bt(e,r.id),await Qe(r.id,{isStale:()=>n!==Y,logPrefix:`loadAgentSessions`})}else{let t=await be(e,`web-chat-`+Date.now());if(n!==Y)return;let[r,i]=await Promise.all([L(e,{includeDms:!1}),wt()]);if(n!==Y)return;y(gt(r.sessions||[]),i),E.value=t.session_id,Le([],t.session_id),Pe(t.session_id,[]),Ee(t.session_id)}}catch(e){if(n!==Y)return;console.error(`[loadAgentSessions] failed:`,e)}}async function Et(e,t){if(!H.value.find(t=>t.id===e))return;Oe(),lt(),qe(),V.value=e,S.value=dt(e),localStorage.setItem(vt,e),Ve.value=!0,E.value=null,Ie.value=null,h(),Le([]),O.value=[],G.value=null,K.value=null,he();let n=Tt(e,t&&t.targetSessionId),r=Y;try{await n}finally{r===Y&&(Ve.value=!1)}}var X=o(null),Z=o(`agents`);function Dt(e){X.value===e?X.value=null:(X.value=e,Z.value=e)}var Ot=`alms_theme`;function kt(){return localStorage.getItem(Ot)||`dark`}var At=o(kt());function jt(){let e=At.value===`dark`?`light`:`dark`;At.value=e,localStorage.setItem(Ot,e),document.documentElement.setAttribute(`data-theme`,e)}document.documentElement.setAttribute(`data-theme`,kt());var Mt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="8"/><path d="M10 6v4l3 3"/></svg>`,Nt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 5a2 2 0 012-2h3l2 2h5a2 2 0 012 2v7a2 2 0 01-2 2H5a2 2 0 01-2-2V5z"/></svg>`,Pt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M5 5l10 10M15 5L5 15"/></svg>`,Ft=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 15V5M10 5L5 10M10 5l5 5"/></svg>`,It=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor"><rect x="5" y="5" width="10" height="10" rx="1.5"/></svg>`,Lt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l10 6-10 6V4z"/></svg>`,Rt=()=>d`<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 00-2 2v.18a2 2 0 01-1 1.73l-.43.25a2 2 0 01-2 0l-.15-.08a2 2 0 00-2.73.73l-.22.38a2 2 0 00.73 2.73l.15.1a2 2 0 011 1.72v.51a2 2 0 01-1 1.74l-.15.09a2 2 0 00-.73 2.73l.22.38a2 2 0 002.73.73l.15-.08a2 2 0 012 0l.43.25a2 2 0 011 1.73V20a2 2 0 002 2h.44a2 2 0 002-2v-.18a2 2 0 011-1.73l.43-.25a2 2 0 012 0l.15.08a2 2 0 002.73-.73l.22-.39a2 2 0 00-.73-2.73l-.15-.08a2 2 0 01-1-1.74v-.5a2 2 0 011-1.74l.15-.09a2 2 0 00.73-2.73l-.22-.38a2 2 0 00-2.73-.73l-.15.08a2 2 0 01-2 0l-.43-.25a2 2 0 01-1-1.73V4a2 2 0 00-2-2z"/><circle cx="12" cy="12" r="3"/></svg>`,zt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M3 5h14M3 10h14M3 15h14"/></svg>`,Bt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="10" cy="10" r="4"/><path d="M10 2v2M10 16v2M3.5 10H2M18 10h-1.5M5.05 5.05L3.63 3.63M16.37 16.37l-1.42-1.42M5.05 14.95l-1.42 1.42M16.37 3.63l-1.42 1.42"/></svg>`,Vt=()=>d`<svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 12.5A7.5 7.5 0 017.5 3 7.5 7.5 0 1017 12.5z"/></svg>`,Ht=o(!1);function Ut(){Ht.value=!Ht.value}function Wt(){Ht.value=!1}var Gt=[`agents`,`jobs`,`audit`],Kt=s(()=>W.value.posture||`guarded`);function qt({onOpenSettings:e,status:t}){let n=Kt.value,r=t.value===`connected`?`ok`:t.value===`running`?`running`:t.value===`error`||t.value===`offline`?`error`:``;return d`
        <header>
            <button class="sidebar-toggle-btn" title="Toggle sessions" aria-label="Toggle sessions"
                    onClick=${Ut}>
                ${Ht.value?d`<${Pt} />`:d`<${zt} />`}
            </button>
            <h1>ALMS</h1>

            ${n===`guarded`&&d`
                <span id="posture-badge" class="guarded">guarded</span>
            `}
            ${n===`autonomous`&&d`
                <span id="posture-badge" class="autonomous">autonomous</span>
            `}

            <div class="header-spacer"></div>

            <span class="status-dot ${r}" aria-hidden="true"></span>
            <span id="status">${t.value}</span>
            ${He.value&&d`
                <button class="retry-btn" onClick=${Ue}>Retry</button>
            `}

            <div class="header-btns">
                ${Gt.map(e=>d`
                    <button class="hbtn ${X.value===e?`active`:``}"
                            onClick=${()=>Dt(e)}>
                        ${e.charAt(0).toUpperCase()+e.slice(1)}
                    </button>
                `)}
            </div>

            <button class="header-icon-btn" title="Toggle theme" aria-label="Toggle theme"
                    onClick=${jt}>
                ${At.value===`dark`?d`<${Bt} />`:d`<${Vt} />`}
            </button>

            <button class="header-icon-btn settings-btn" title="Settings" aria-label="Settings"
                    onClick=${e}>
                <${Rt} />
            </button>
        </header>
    `}async function Jt(t,n){if(!t||t===E.value)return;Wt();let r=m.value.find(e=>e.id===t)||N.value.find(e=>e.id===t);if(r&&r.session_type!==`dm`&&r.session_type!==`notification`&&r.session_type!==`job`&&r.agent_id&&V.value&&r.agent_id!==V.value&&H.value.some(e=>e.id===r.agent_id)){await Et(r.agent_id,{targetSessionId:t});return}let i=qe();Oe(),e(()=>{E.value=t,Ne(),Ie.value=null,Le([],t),O.value=[],K.value=null,he(),F.value=null,We.value=!0}),bt(V.value,t);try{await Qe(t,{isStale:()=>i!==Ke,logPrefix:n&&n.logPrefix||`navigateToSession`})}finally{i===Ke&&(We.value=!1)}}var Yt=`alms.composer.draft.`,Xt=`alms.composer.queue.`,Zt=64*1024,Qt=50,$t=256*1024;function en(){try{return typeof window<`u`&&!!window.localStorage}catch{return!1}}function tn(e){return Yt+e}function nn(e){return Xt+e}function rn(e){if(!e||!en())return``;try{let t=localStorage.getItem(tn(e));return typeof t==`string`?t:``}catch{return``}}function an(e,t){if(!(!e||!en())){if(!t){try{localStorage.removeItem(tn(e))}catch{}return}try{localStorage.setItem(tn(e),t)}catch{try{let n=t.length>Zt?t.slice(0,Zt):t;localStorage.setItem(tn(e),n),n.length<t.length&&console.warn(`[composer-storage] draft truncated from %d to %d chars on save (storage quota)`,t.length,n.length)}catch{console.warn(`[composer-storage] draft not persisted (storage rejected both full and truncated writes); in-memory textarea is still authoritative`)}}}}function on(e){if(!(!e||!en()))try{localStorage.removeItem(tn(e))}catch{}}function sn(e){if(!e||!en())return[];try{let t=localStorage.getItem(nn(e));if(!t)return[];let n=JSON.parse(t);return Array.isArray(n)?n.slice(0,Qt).filter(e=>e&&typeof e.text==`string`).map(e=>({text:e.text})):[]}catch{return[]}}function cn(e,t){if(!(!e||!en()))try{if(!Array.isArray(t)||t.length===0){localStorage.removeItem(nn(e));return}let n=t.slice(0,Qt).map(e=>({text:e.text})),r=JSON.stringify(n);for(;r.length>$t&&n.length>0;)n=n.slice(0,-1),r=JSON.stringify(n);if(n.length===0){localStorage.removeItem(nn(e));return}localStorage.setItem(nn(e),r)}catch{}}function ln(e){if(!(!e||!en()))try{localStorage.removeItem(nn(e))}catch{}}function un(e){on(e),ln(e)}function dn(e,t,n){return!n||!Array.isArray(e)||e[0]!==t?e:e.slice(1)}function fn({restoredQueue:e,activeRunId:t,activeAgentId:n}){return!Array.isArray(e)||e.length===0?{drain:!1,head:null,remaining:[]}:t||!n?{drain:!1,head:null,remaining:e}:{drain:!0,head:e[0],remaining:e.slice(1)}}var pn={chat:{icon:`▸`,cls:``,label:`Chat session`},dm:{icon:`↔`,cls:`dm`,label:`DM conversation`},notification:{icon:`⚡`,cls:`notification`,label:`Notification session`},job:{icon:`⏰`,cls:`job`,label:`Job session`},subagent:{icon:`⚙`,cls:`subagent`,label:`Subagent session`},telegram:{icon:`✉`,cls:`telegram`,label:`Telegram session`}};function mn(e){return pn[e.session_type]||pn.chat}function hn(e,t,n,r){if(e===t&&n)return!0;let i=r[e];return!!(i&&!i.finished)}function gn(e){return Jt(e,{logPrefix:`selectSession`})}async function _n(){if(V.value){Oe(),qe();try{let t=`web-chat-`+Date.now(),n=await be(V.value,t),[r,i]=await Promise.all([L(V.value,{includeDms:!0}),wt()]);e(()=>{y(r.sessions||[],i),E.value=n.session_id,bt(V.value,n.session_id),Ie.value=null,Le([],n.session_id),O.value=[],Pe(n.session_id,[]),K.value=null,he()}),Ee(n.session_id)}catch(e){console.error(`[newSession] failed:`,e)}}}function vn(e){let t=e.participants;return Array.isArray(t)&&t.length>=2?t.join(` <-> `):e.context_id||e.id.slice(0,8)}function yn(e){return e.agent_name?`notifications`:e.context_id||e.id.slice(0,8)}function bn(e){let t=e.context_id||``;return t.startsWith(`job_`)&&t.length>4?`job `+t.slice(4,12):t||e.id.slice(0,8)}function xn(e){return e.session_type===`dm`?vn(e):e.session_type===`notification`?yn(e):e.session_type===`job`?bn(e):e.context_id||e.id.slice(0,8)}function Sn(e){if(e.session_type===`notification`&&e.agent_name)return e.agent_name;if(e.session_type===`job`&&e.agent_id){let t=H.value.find(t=>t.id===e.agent_id);return t?t.name:null}return null}function Cn({session:t,activeAgentName:n}){let r=a(!1),i=a(null),o=E.value,s=t.id===o,c=hn(t.id,o,U.value,w.value),l=mn(t),u=l.cls?` session-item-`+l.cls:``,f=e=>{e.stopPropagation(),r.value=!0,i.value=setTimeout(()=>{r.value=!1},3e3)},p=async n=>{n.stopPropagation(),i.value&&=(clearTimeout(i.value),null),r.value=!1;try{await ue(t.id),un(t.id),Be(t.id),Ne(t.id),t.id===E.value&&(Oe(),e(()=>{E.value=null,Ie.value=null,K.value=null,he(),O.value=[]}));let[n,r]=await Promise.all([L(V.value,{includeDms:!0}),wt()]);y(n.sessions||[],r)}catch(e){console.error(`[deleteSession] failed:`,e)}},m=e=>{e.stopPropagation(),i.value&&=(clearTimeout(i.value),null),r.value=!1},h=xn(t),g=t.session_type===`chat`?``:`
Type: `+t.session_type,_=Sn(t);return d`
        <div class="session-item${u}${mt(t,n)?` session-item-active-agent`:``} ${s?`active`:``} ${c?`has-run`:``}"
             role="option"
             aria-selected=${s}
             tabindex="0"
             title=${`ID: `+t.id+`
Context: `+t.context_id+g}
             onClick=${()=>gn(t.id)}
             onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),gn(t.id))}}>
            ${t.session_type!==`chat`&&d`<span class="session-type-icon session-type-icon-${l.cls||`default`}" aria-hidden="true" title=${l.label}>${l.icon}</span>`}
            <span class="session-label">${h}</span>
            ${_&&d`<span class="session-agent-attribution" title=${`Owned by `+_}>${_}</span>`}
            ${r.value?d`
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
                `:d`
                    <button class="session-delete-btn"
                            title="Delete session"
                            aria-label="Delete session"
                            onClick=${f}
                            onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),f(e))}}>\u00D7</button>
                `}
        </div>
    `}function wn({label:e,cls:t,id:n}){return d`
        <div class="session-section-divider ${t||``}" role="presentation" id=${n}>
            <span class="session-section-divider-label">${e}</span>
        </div>
    `}function Tn({expanded:e,count:t,headerId:n}){let r=e=>{e.stopPropagation(),oe.value=!oe.value};return d`
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
    `}function En(e){let t=ut(S.value,e);S.value=t,t&&t!==V.value&&Et(t)}function Dn({agent:e,expanded:t,sessionCount:n,isActive:r,headerId:i}){let a=t=>{t.stopPropagation(),En(e.id)},o=t=>{(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),En(e.id))},s=r?t?`Collapse sessions`:`Expand sessions`:`Switch to `+e.name;return d`
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
            ${n!=null&&d`
                <span class="agent-group-count" title=${n+` session`+(n===1?``:`s`)}>
                    ${n}
                </span>
            `}
        </div>
    `}function On(e){let t=new Set,n=[];for(let r of e){if(r.session_type!==`dm`||!Array.isArray(r.participants)||r.participants.length<2)continue;let e=r.context_id||r.id;t.has(e)||(t.add(e),n.push(r))}return n}function kn(){let e=m.value,t=N.value,n=H.value,r=V.value,i=S.value,a=B.value?B.value.name:null,o=pt(e.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`&&e.session_type!==`subagent`&&e.session_type!==`episodic`)),s=pt(t.filter(e=>e.session_type!==`dm`&&e.session_type!==`notification`&&e.session_type!==`job`)),c=ht(On(t),a),l=ht(t.filter(e=>e.session_type===`notification`),a),u=_t(t),f=oe.value;return d`
        <div class="sidebar-section" style="flex:1; min-height:0">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${(!n||n.length===0)&&c.length===0&&l.length===0&&u.length===0?d`<div class="empty-state">No sessions</div>`:null}
                ${n.map(e=>{let t=ft(i,e.id),n=e.id===r,c=o.get(e.id)||[],l=n?c.length:(s.get(e.id)||[]).length,u=`agent-group-header-`+e.id;return d`
                        <div class="agent-group" key=${e.id}>
                            <${Dn}
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
                                    ${c.length===0?d`<div class="empty-state agent-group-empty">No sessions</div>`:c.map(e=>d`
                                            <${Cn} key=${e.id} session=${e} activeAgentName=${a} />
                                        `)}
                                </div>
                            </div>
                        </div>
                    `})}
                ${c.length>0&&d`
                    <${wn} label="Direct messages"
                                       cls="session-divider-dm"
                                       id="session-section-dms" />
                    <div role="group" aria-labelledby="session-section-dms">
                        ${c.map(e=>d`
                            <${Cn} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${l.length>0&&d`
                    <${wn} label="Notifications"
                                       cls="session-divider-notification"
                                       id="session-section-notifications" />
                    <div role="group" aria-labelledby="session-section-notifications">
                        ${l.map(e=>d`
                            <${Cn} key=${e.id} session=${e} activeAgentName=${a} />
                        `)}
                    </div>
                `}
                ${u.length>0&&d`
                    <${Tn} expanded=${f}
                                          count=${u.length}
                                          headerId="session-section-jobs" />
                    <div class="agent-group-body"
                         role="group"
                         aria-labelledby="session-section-jobs"
                         data-expanded=${f}>
                        <div class="agent-group-sessions">
                            ${u.map(e=>d`
                                <${Cn} key=${e.id} session=${e} activeAgentName=${a} />
                            `)}
                        </div>
                    </div>
                `}
            </div>
            <button id="new-session-btn" onClick=${_n}>+ New session</button>
        </div>
    `}function An(){let e=Ht.value?` sidebar-open`:``;return d`
        ${Ht.value&&d`<div class="sidebar-backdrop" onClick=${Wt}></div>`}
        <div id="sidebar" class=${e}>
            <${kn} />
        </div>
    `}function jn(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function Mn(e){if(!e)return``;let t=new Date(e),n=new Date;return t.toDateString()===n.toDateString()?jn(e):t.toLocaleDateString([],{month:`short`,day:`numeric`})}function Nn(e){if(!e)return``;let t=new Date(e);if(isNaN(t.getTime()))return``;let n=new Date,r=t.toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`});return t.toDateString()===n.toDateString()?r:`${t.toLocaleDateString([],{month:`short`,day:`numeric`})} ${r}`}function Pn(e){e&&(e.scrollTop=e.scrollHeight)}function Fn(e){if(!e)return``;let t=``;if(typeof e.querySelector==`function`){let n=e.querySelector(`code`);n&&typeof n.textContent==`string`&&(t=n.textContent)}return!t&&typeof e.textContent==`string`&&(t=e.textContent),t?(t.endsWith(`\r
`)?t=t.slice(0,-2):t.endsWith(`
`)&&(t=t.slice(0,-1)),t):``}function In(){return!!(typeof navigator<`u`&&navigator.clipboard&&typeof navigator.clipboard.writeText==`function`)}var Ln=`cb-copy-decorated`,Rn=`code-block-wrapper`,zn=`code-block-copy`,Bn=`code-block-copy--copied`,Vn=`alms-code-copy-live`;function Hn(){if(typeof document>`u`)return null;let e=document.getElementById(Vn);return e||(e=document.createElement(`div`),e.id=Vn,e.setAttribute(`aria-live`,`polite`),e.setAttribute(`role`,`status`),e.style.position=`absolute`,e.style.width=`1px`,e.style.height=`1px`,e.style.padding=`0`,e.style.margin=`-1px`,e.style.overflow=`hidden`,e.style.clip=`rect(0, 0, 0, 0)`,e.style.whiteSpace=`nowrap`,e.style.border=`0`,document.body.appendChild(e),e)}function Un(){let e=Hn();e&&(e.textContent=``,setTimeout(()=>{e.textContent=`Copied to clipboard`},50))}function Wn(){return[`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" `,`stroke="currentColor" stroke-width="1.5" stroke-linecap="round" `,`stroke-linejoin="round" aria-hidden="true">`,`<rect x="7" y="7" width="10" height="10" rx="1.5"/>`,`<path d="M5 13H4a1 1 0 01-1-1V4a1 1 0 011-1h8a1 1 0 011 1v1"/>`,`</svg>`].join(``)}function Gn(){return[`<svg width="14" height="14" viewBox="0 0 20 20" fill="none" `,`stroke="currentColor" stroke-width="2" stroke-linecap="round" `,`stroke-linejoin="round" aria-hidden="true">`,`<path d="M4 10l4 4 8-8"/>`,`</svg>`].join(``)}function Kn(e){if(typeof document>`u`)return!1;let t=document.createElement(`textarea`);t.value=e,t.style.position=`fixed`,t.style.top=`-9999px`,t.style.left=`-9999px`,t.setAttribute(`readonly`,``),t.setAttribute(`aria-hidden`,`true`),document.body.appendChild(t);let n=!1;try{t.select(),t.setSelectionRange(0,e.length),n=document.execCommand&&document.execCommand(`copy`)}catch{n=!1}return document.body.removeChild(t),!!n}function qn(e){e&&(e._copyRevertTimer&&=(clearTimeout(e._copyRevertTimer),null),e.classList.add(Bn),e.innerHTML=Gn(),e.setAttribute(`aria-label`,`Copied`),e.title=`Copied`,Un(),e._copyRevertTimer=setTimeout(()=>{e.classList.remove(Bn),e.innerHTML=Wn(),e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e._copyRevertTimer=null},1500))}function Jn(e,t,n){e.preventDefault(),e.stopPropagation();let r=Fn(t);if(r){if(In()){navigator.clipboard.writeText(r).then(()=>qn(n),()=>{Kn(r)&&qn(n)});return}Kn(r)&&qn(n)}}function Yn(e,t=`pre`){if(!e||typeof e.querySelectorAll!=`function`)return;let n=e.querySelectorAll(`.${Rn}`);for(let e=0;e<n.length;e++){let t=n[e];if(!t.parentNode)continue;let r=t.querySelector(`pre`);if(!r){t.parentNode.removeChild(t);continue}if(!r.classList.contains(Ln)){let e=t.parentNode,n=Array.from(t.childNodes);for(let r=0;r<n.length;r++){let i=n[r];i.nodeType===1&&i.classList&&i.classList.contains(zn)||e.insertBefore(i,t)}e.removeChild(t)}}let r=e.querySelectorAll(t);for(let e=0;e<r.length;e++){let t=r[e];if(t.classList.contains(Ln))continue;let n=t.parentNode;if(!n)continue;if(n.classList&&n.classList.contains(Rn)){if(!n.querySelector(`.${zn}`)){let e=document.createElement(`button`);e.type=`button`,e.className=zn,e.setAttribute(`aria-label`,`Copy code`),e.title=`Copy code`,e.innerHTML=Wn(),e.addEventListener(`click`,n=>Jn(n,t,e)),n.appendChild(e)}t.classList.add(Ln);continue}if(!((t.textContent||``).trim().length>0)){t.classList.add(Ln);continue}let i=document.createElement(`div`);i.className=Rn,n.insertBefore(i,t),i.appendChild(t);let a=document.createElement(`button`);a.type=`button`,a.className=zn,a.setAttribute(`aria-label`,`Copy code`),a.title=`Copy code`,a.innerHTML=Wn(),a.addEventListener(`click`,e=>Jn(e,t,a)),i.appendChild(a),t.classList.add(Ln)}}function Xn({ts:e}){if(!e)return null;let t=Nn(e);return t?d`<span class="msg-timestamp" title=${e}>${t}</span>`:null}function Zn({text:e,live:t}){let n=a(!1);if(!e)return null;let r=()=>{n.value=!n.value},i=e.length>0?` (${e.length} chars)`:``,o=t?`Thinking…`:`Reasoning`,s=n.value?`▼`:`▶`;return d`
        <div class="reasoning-panel ${t?`reasoning-panel--live`:``} ${n.value?`reasoning-panel--open`:``}">
            <button class="reasoning-panel-toggle" onClick=${r}
                    aria-expanded=${n.value}>
                <span class="reasoning-panel-arrow">${s}</span>
                <span class="reasoning-panel-glyph">\u{1F4AD}</span>
                <span class="reasoning-panel-title">${o}</span>
                <span class="reasoning-panel-hint">${i}</span>
            </button>
            ${n.value&&d`
                <div class="reasoning-panel-body">
                    <pre class="reasoning-panel-text">${e}</pre>
                </div>
            `}
        </div>
    `}function Qn({html:e}){let t=c(null);return i(()=>{Yn(t.current)},[e]),d`
        <div class="msg-body markdown-body" ref=${t}
             dangerouslySetInnerHTML=${{__html:e}} />
    `}function $n({type:e,role:t,text:n,sealed:r,fromAgent:i,reasoning:a,ts:o}){let s=e===`user`?`user`:`agent`,c=b.value||B.value?.name,l=e===`user`?`>`:i?`${i} $`:c?`${c} $`:`$`,f=e===`agent`&&r===!1,p=e===`agent`&&a?d`<${Zn} text=${a} live=${f} />`:null,m=typeof n==`string`&&n.trim().length>0,h=o&&!f;if(e===`agent`&&r){let e=m?u(n):``;return d`
            <div class="msg ${s}">
                <div class="msg-label-row">
                    <div class="msg-label">${l}</div>
                    ${h&&d`<${Xn} ts=${o} />`}
                </div>
                ${p}
                ${m&&d`<${Qn} html=${e} />`}
            </div>
        `}return d`
        <div class="msg ${s}">
            <div class="msg-label-row">
                <div class="msg-label">${l}</div>
                ${h&&d`<${Xn} ts=${o} />`}
            </div>
            ${p}
            ${(m||f)&&d`
                <div class="msg-body ${f?`streaming-cursor`:``}">${n}</div>
            `}
        </div>
    `}function er({usage:e}){if(!e)return null;let t=e.prompt_tokens||0,n=e.completion_tokens||0;if(t+n===0)return null;let r=e.reasoning_tokens;return d`<div class="msg-tokens">${t}p + ${n}c${typeof r==`number`&&r>0?` + ${r}r`:``} tokens</div>`}function tr({text:e,code:t}){return d`
        <div class="msg msg-error ${t?`msg-error--${t.toLowerCase()}`:``}" data-code=${t||``}>
            <div class="msg-error-icon">\u274C</div>
            <div class="msg-error-body">
                <div class="msg-error-title">Error</div>
                <div class="msg-error-text">${e}</div>
            </div>
        </div>
    `}function nr({id:e,text:t,code:n}){let r=a(!1),i=a(!1);return i.value?null:d`
        <div class="msg msg-warning ${r.value?`msg-warning--collapsed`:``}" data-code=${n||``}>
            <div class="msg-warning-icon">\u26A0\uFE0F</div>
            <div class="msg-warning-body">
                <div class="msg-warning-header" onClick=${()=>{r.value=!r.value}}>
                    <div class="msg-warning-title">Warning</div>
                    ${n&&d`<span class="msg-warning-code">${n}</span>`}
                    <button class="msg-warning-toggle"
                            title=${r.value?`Expand`:`Collapse`}
                            aria-label=${r.value?`Expand warning`:`Collapse warning`}
                            aria-expanded=${!r.value}>
                        ${r.value?`▶`:`▼`}
                    </button>
                    <button class="msg-warning-dismiss" onClick=${t=>{t.stopPropagation(),i.value=!0,e&&Re(t=>t.id!==e)}}
                            title="Dismiss" aria-label="Dismiss warning">
                        \u2715
                    </button>
                </div>
                ${!r.value&&d`
                    <div class="msg-warning-text">${t}</div>
                `}
            </div>
        </div>
    `}function rr({text:e}){return d`
        <div class="msg-system">
            ${e}
        </div>
    `}function ir({status:e,error:t}){return!e||e===`completed`?d`<div class="run-boundary run-boundary--completed" />`:d`
        <div class="run-boundary ${e===`failed`?`run-boundary--failed`:e===`cancelled`?`run-boundary--cancelled`:``}">
            <span class="run-boundary-label">${e===`failed`?`run failed`:e===`cancelled`?`run cancelled`:`run ${e}`}</span>
        </div>
        ${e===`failed`&&t&&d`
            <div class="run-boundary-error">${t}</div>
        `}
    `}function ar({peer:e,reason:t}){return d`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">DM conversation with ${e} ended</span>
            <span class="dm-ended-reason">${t}</span>
        </div>
    `}function or(e,t){if(!t)return``;switch(e){case`shell`:case`shell_exec`:return t.command?t.command:t.argv?t.argv.join(` `):``;case`fs_read`:return t.path||``;case`fs_write`:return`${t.mode===`append`?`(append) `:``}${t.path||``}`;case`fs_list`:return t.path||`.`;case`workspace_write`:return`${t.file||``}: ${(t.content||``).slice(0,60)}`;case`http_get`:if(!t.url)return``;try{return new URL(t.url).hostname+` `+t.url}catch{return t.url}case`math`:return t.operation?t.operation+`(`+[t.a,t.b,t.n].filter(e=>e!==void 0).join(`, `)+`)`:``;case`echo`:return t.message||t.text||``;case`send_message`:return t.to?`to ${t.to}`:``;case`invoke_agent`:{let e=t.name||t.subagent_name||``,n=t.task||``;return e&&n?`${e}: ${n.length>60?n.slice(0,60)+`…`:n}`:e}case`read_session`:return(t.session_id?t.session_id.slice(0,8)+`…`:``)+(t.last_n?` (last ${t.last_n})`:``);case`read_subagent_session`:return(t.name||``)+(t.last_n?` (last ${t.last_n})`:``);case`list_agents`:case`list_my_sessions`:return``;case`read_messages`:return t.from?`from ${t.from}`:``;case`ignore_message`:return t.from?`from ${t.from}`:``;default:{let e=Object.entries(t);return e.map(([t,n])=>{let r=typeof n==`string`?n:JSON.stringify(n);return e.length>1?`${t}=${r}`:r}).join(` `)}}}function sr(e){return e<1024?e+` B`:e<1024*1024?(e/1024).toFixed(1)+` KB`:(e/(1024*1024)).toFixed(1)+` MB`}var cr=2e3,lr=800;function ur(e){if(!e)return``;let t=e.replace(/\\/g,`/`).split(`/`).filter(Boolean);return t.length<=2?t.join(`/`):`…/`+t.slice(-2).join(`/`)}function Q(e){return typeof e==`object`&&!!e&&typeof e.error==`string`}function dr(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.task_id==`string`?e.task_id:null,n=typeof e.status==`string`?e.status:null,r=typeof e.command==`string`?e.command:null,i=typeof e.exit_code==`number`?e.exit_code:null,a=typeof e.stdout==`string`?e.stdout:``,o=typeof e.stderr==`string`?e.stderr:``,s=typeof e.error==`string`?e.error:null,c=typeof e.message==`string`?e.message:null;return t&&(n===`submitted`||n===`unknown`||n===`not_found_or_still_running`)?d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Status</div>
                <div class="tc-status-row">
                    <span class="tc-kv-badge">${n}</span>
                    <span class="tc-kv-mono">task_id: ${t}</span>
                </div>
                ${c&&d`
                    <pre class="tc-detail-content">${c}</pre>
                `}
            </div>
        `:t&&n===`failed`&&s?d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Status</div>
                <div class="tc-status-row">
                    <span class="tc-kv-badge tc-kv-badge-fail">${n}</span>
                    ${r&&d`<span class="tc-kv-mono">${r}</span>`}
                </div>
            </div>
            <div class="tc-detail-section">
                <div class="tc-detail-label">Error</div>
                <pre class="tc-detail-content tc-detail-error">${s}</pre>
            </div>
        `:i==null&&!a&&!o?null:d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Status</div>
            <div class="tc-status-row">
                <span class="${i===0||i==null?`tc-kv-badge`:`tc-kv-badge tc-kv-badge-fail`}">
                    exit ${i??`?`}
                </span>
                ${t&&d`<span class="tc-kv-mono">task_id: ${t}</span>`}
            </div>
        </div>
        ${a&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">stdout</div>
                <pre class="tc-detail-content tc-code-block">${a}</pre>
            </div>
        `}
        ${o&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">stderr</div>
                <pre class="tc-detail-content tc-code-block tc-detail-warn">${o}</pre>
            </div>
        `}
    `}function fr(e,t){if(typeof e!=`object`||!e||Q(e)||typeof e.content!=`string`)return null;let n=t&&t.path||``,r=typeof e.lines_returned==`number`?e.lines_returned:null,i=typeof e.total_lines==`number`?e.total_lines:null,a=e.has_more_before===!0,o=e.has_more_after===!0,s=typeof e.note==`string`?e.note:null,c=e.byte_budget_exceeded===!0,l=e.line_truncated===!0,u=[];r!=null&&i!=null?r===i?u.push(`${r} lines (full file)`):u.push(`${r} of ${i} lines`):r!=null&&u.push(`${r} lines`),a&&u.push(`more before`),o&&u.push(`more after`),c&&u.push(`byte-budget exceeded`),l&&u.push(`per-line truncated`);let f=u.join(` · `),p=e.content||``;return d`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">
                ${n?ur(n):`File content`}
            </div>
            ${p?d`<pre class="tc-detail-content tc-code-block">${p}</pre>`:d`<pre class="tc-detail-content tc-detail-muted">${s||`(empty)`}</pre>`}
            ${f&&d`<div class="tc-detail-footer">${f}</div>`}
            ${p&&s&&d`<div class="tc-detail-footer">${s}</div>`}
        </div>
    `}function pr(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.path==`string`&&e.path||typeof e.file==`string`&&e.file||null,n=typeof e.replacements==`number`?e.replacements:null,r=typeof e.mode==`string`?e.mode:null,i=e.ok===!0;return t?d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge ${i?``:`tc-kv-badge-fail`}">
                    ${i?`ok`:`failed`}
                </span>
                <span class="tc-kv-mono">${t}</span>
                ${n!=null&&d`
                    <span class="tc-kv-meta">
                        ${n} ${n===1?`replacement`:`replacements`}
                    </span>
                `}
                ${r&&d`
                    <span class="tc-kv-meta">${r}</span>
                `}
            </div>
        </div>
    `:null}function mr(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.matches))return null;let t=e.matches,n=e.truncated===!0,r=typeof e.truncated_lines==`number`&&e.truncated_lines>0?e.truncated_lines:0;if(t.length===0)return d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Matches</div>
                <div class="tc-detail-footer">No matches found.</div>
            </div>
        `;let i=t[0],a;if(typeof i==`string`)a=d`
            <ul class="tc-match-list">
                ${t.map(e=>d`
                    <li class="tc-match-row tc-match-files">
                        <span class="tc-match-path">${e}</span>
                    </li>
                `)}
            </ul>
        `;else if(i&&typeof i.count==`number`&&typeof i.file==`string`)a=d`
            <ul class="tc-match-list">
                ${t.map(e=>d`
                    <li class="tc-match-row tc-match-count">
                        <span class="tc-match-path">${e.file}</span>
                        <span class="tc-kv-meta">${e.count}</span>
                    </li>
                `)}
            </ul>
        `;else if(i&&typeof i.file==`string`&&typeof i.line==`number`)a=d`
            <ul class="tc-match-list">
                ${t.map(e=>{let t=Array.isArray(e.context_before)?e.context_before:[],n=Array.isArray(e.context_after)?e.context_after:[];return d`
                        <li class="tc-match-row tc-match-content">
                            <div class="tc-match-loc">
                                <span class="tc-match-path">${e.file}</span>
                                <span class="tc-match-sep">:</span>
                                <span class="tc-match-line">${e.line}</span>
                            </div>
                            ${t.length>0&&d`
                                <pre class="tc-match-snippet tc-match-context">${t.join(`
`)}</pre>
                            `}
                            <pre class="tc-match-snippet">${e.content||``}</pre>
                            ${n.length>0&&d`
                                <pre class="tc-match-snippet tc-match-context">${n.join(`
`)}</pre>
                            `}
                        </li>
                    `})}
            </ul>
        `;else return null;let o=typeof e.total_matches==`number`?e.total_matches:null,s=typeof e.total==`number`?e.total:null,c=[];return o==null?s!=null&&c.push(`${s} match${s===1?``:`es`}`):c.push(`${o} match${o===1?``:`es`}`),n&&c.push(`output truncated`),r>0&&c.push(`${r} per-line truncated`),d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Matches</div>
            ${a}
            ${c.length>0&&d`
                <div class="tc-detail-footer">${c.join(` · `)}</div>
            `}
        </div>
    `}function hr(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.files))return null;let t=e.files,n=typeof e.total==`number`?e.total:t.length,r=e.truncated===!0;if(t.length===0)return d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Files</div>
                <div class="tc-detail-footer">No files matched.</div>
            </div>
        `;let i=[];return n===t.length?i.push(`${n} file${n===1?``:`s`}`):i.push(`${t.length} of ${n} files`),r&&i.push(`output truncated`),d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Files</div>
            <ul class="tc-match-list">
                ${t.map(e=>d`
                    <li class="tc-match-row tc-match-files">
                        <span class="tc-match-path">${e}</span>
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">${i.join(` · `)}</div>
        </div>
    `}function gr(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.entries))return null;let t=typeof e.path==`string`?e.path:``,n=e.entries;return n.length===0?d`
            <div class="tc-detail-section">
                <div class="tc-detail-label tc-file-header">${t||`/`}</div>
                <div class="tc-detail-footer">Empty directory.</div>
            </div>
        `:d`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">${t||`/`}</div>
            <ul class="tc-match-list">
                ${n.map(e=>d`
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
    `}function _r(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.status==`number`?e.status:null,a=typeof e.content_type==`string`?e.content_type:null,o=e.body;if(i==null&&o===void 0)return null;let s=a&&a.toLowerCase().includes(`application/json`),c;if(typeof o==`string`)c=o;else if(o==null)c=``;else try{c=JSON.stringify(o,null,2)}catch{c=String(o)}let l=c.length>cr,u=r&&!r.value&&l?c.slice(0,cr)+`…`:c,f=e=>{e.stopPropagation(),r&&(r.value=!r.value)},p=i!=null&&i>=200&&i<400?`tc-kv-badge`:`tc-kv-badge tc-kv-badge-fail`,m=[];if(e.headers&&typeof e.headers==`object`&&!Array.isArray(e.headers)){let t=Object.keys(e.headers).sort();for(let n of t){let t=e.headers[n];if(Array.isArray(t))for(let e of t)m.push([n,typeof e==`string`?e:String(e)]);else typeof t==`string`&&m.push([n,t])}}let h=m.length;return d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Response</div>
            <div class="tc-status-row">
                ${i!=null&&d`
                    <span class="${p}">${i}</span>
                `}
                ${a&&d`
                    <span class="tc-kv-meta">${a}</span>
                `}
            </div>
        </div>
        ${c&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">
                    Body${s?` (JSON)`:``}
                </div>
                <pre class="tc-detail-content tc-code-block">${u}</pre>
                ${l&&r&&d`
                    <button class="tc-show-more" onClick=${f}>
                        ${r.value?`Show less`:`Show more`}
                    </button>
                `}
            </div>
        `}
        ${h>0&&d`
            <details class="tc-detail-section tc-http-headers">
                <summary class="tc-detail-label tc-http-headers-summary">
                    Headers (${h})
                </summary>
                <ul class="tc-record-list tc-http-headers-list">
                    ${m.map(([e,t])=>d`
                        <li class="tc-record-row tc-http-header-row">
                            <span class="tc-kv-mono tc-http-header-key">${e}</span>
                            <span class="tc-kv-meta tc-http-header-value">${t}</span>
                        </li>
                    `)}
                </ul>
            </details>
        `}
    `}function vr(e,t,n){let r=n&&n.showFull;if(typeof e!=`object`||!e||Q(e))return null;let i=typeof e.task_id==`string`?e.task_id:null,a=typeof e.session_id==`string`?e.session_id:null,o=typeof e.response==`string`?e.response:``,s=t&&(t.name||t.subagent_name)||``;if(i)return d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Subagent (background)</div>
                <div class="tc-status-row">
                    ${s&&d`<span class="tc-kv-badge">${s}</span>`}
                    <span class="tc-kv-mono">task_id: ${i}</span>
                </div>
                ${a&&d`
                    <button class="tc-detail-link"
                        type="button"
                        onClick=${e=>{e.stopPropagation(),Jt(a,{logPrefix:`invokeAgentLink`})}}>
                        View full session
                    </button>
                `}
            </div>
        `;if(!o&&!a)return null;let c=o.length>lr,l=r&&!r.value&&c?o.slice(0,lr)+`…`:o;return d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Subagent</div>
            <div class="tc-status-row">
                ${s&&d`<span class="tc-kv-badge">${s}</span>`}
                <span class="tc-kv-meta">completed</span>
            </div>
        </div>
        ${o&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Response</div>
                <pre class="tc-detail-content">${l}</pre>
                ${c&&r&&d`
                    <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),r&&(r.value=!r.value)}}>
                        ${r.value?`Show less`:`Show more`}
                    </button>
                `}
            </div>
        `}
        ${a&&d`
            <div class="tc-detail-section">
                <button class="tc-detail-link"
                    type="button"
                    onClick=${e=>{e.stopPropagation(),Jt(a,{logPrefix:`invokeAgentLink`})}}>
                    View full session
                </button>
            </div>
        `}
    `}function yr(e){if(typeof e!=`object`||!e||Q(e))return null;let t=e.delivered===!0,n=typeof e.dm_session_id==`string`?e.dm_session_id:null,r=typeof e.note==`string`?e.note:null;return!t&&!n?null:d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Delivery</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${t?`delivered`:`pending`}</span>
                ${n&&d`
                    <span class="tc-kv-mono">session: ${n}</span>
                `}
            </div>
            ${r&&d`<div class="tc-detail-footer">${r}</div>`}
        </div>
    `}function br(e,t,n){let r=n&&n.tool;if(typeof e!=`object`||!e||Q(e))return null;let i=Array.isArray(e.messages)?e.messages:null,a=typeof e.summary==`string`&&e.summary.length>0?e.summary:null,o=typeof e.peer==`string`?e.peer:null,s=typeof e.subagent==`string`?e.subagent:null,c=typeof e.session_id==`string`?e.session_id:null,l=typeof e.note==`string`&&e.note.length>0?e.note:null,u=typeof e.message_count==`number`?e.message_count:typeof e.fallback_message_count==`number`?e.fallback_message_count:null,f=typeof e.showing==`number`?e.showing:typeof e.fallback_showing==`number`?e.fallback_showing:i?i.length:null;if(!i&&a){let e=[];return u!=null&&e.push(`${u} messages total`),c&&e.push(`session: ${c.slice(0,8)}…`),d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">
                    ${s?`Subagent ${s}`:`Summary`}
                </div>
                <pre class="tc-detail-content">${a}</pre>
                ${e.length>0&&d`
                    <div class="tc-detail-footer">${e.join(` · `)}</div>
                `}
            </div>
        `}let p=Array.isArray(e.fallback_messages)?e.fallback_messages:null,m=i||p;if(!m)return null;let h=o?`Conversation with ${o}`:s?`Subagent ${s}`:r===`read_session`?`Session messages`:`Messages`,g=[];return f!=null&&u!=null&&f<u?g.push(`showing ${f} of ${u}`):u!=null&&g.push(`${u} messages`),c&&g.push(`session: ${c.slice(0,8)}…`),d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">${h}</div>
            <ul class="tc-chat-list">
                ${m.map(e=>{let t=typeof e.from==`string`&&e.from||typeof e.role==`string`&&e.role||`?`,n=typeof e.content==`string`?e.content:``;return d`
                        <li class="tc-chat-row ${t===`you`||t===`user`?`tc-chat-self`:`tc-chat-peer`}">
                            <span class="tc-chat-sender">${t}</span>
                            <pre class="tc-chat-content">${n}</pre>
                        </li>
                    `})}
            </ul>
            ${l&&d`
                <div class="tc-detail-footer">${l}</div>
            `}
            ${g.length>0&&d`
                <div class="tc-detail-footer">${g.join(` · `)}</div>
            `}
        </div>
        ${a&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Summary</div>
                <pre class="tc-detail-content">${a}</pre>
            </div>
        `}
    `}function xr(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.agents))return null;let t=e.agents;return t.length===0?d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Agents</div>
                <div class="tc-detail-footer">No other agents available.</div>
            </div>
        `:d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Agents</div>
            <ul class="tc-record-list">
                ${t.map(e=>d`
                    <li class="tc-record-row">
                        <div class="tc-record-head">
                            <span class="tc-kv-badge">${e.name||`?`}</span>
                            ${e.last_active&&d`
                                <span class="tc-kv-meta">${e.last_active}</span>
                            `}
                        </div>
                        ${e.description&&d`
                            <div class="tc-record-body">${e.description}</div>
                        `}
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">
                ${t.length} ${t.length===1?`agent`:`agents`}
            </div>
        </div>
    `}function Sr(e){if(typeof e!=`object`||!e||Q(e)||!Array.isArray(e.sessions))return null;let t=e.sessions,n=typeof e.total==`number`?e.total:t.length,r=typeof e.showing==`number`?e.showing:t.length;if(t.length===0)return d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Sessions</div>
                <div class="tc-detail-footer">No sessions found.</div>
            </div>
        `;let i=[];return r<n?i.push(`showing ${r} of ${n}`):i.push(`${n} sessions`),d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Sessions</div>
            <ul class="tc-record-list">
                ${t.map(e=>{let t=typeof e.session_id==`string`?e.session_id.slice(0,8)+`…`:`?`;return d`
                        <li class="tc-record-row">
                            <div class="tc-record-head">
                                ${e.context_type&&d`
                                    <span class="tc-kv-badge">${e.context_type}</span>
                                `}
                                <span class="tc-kv-mono">${t}</span>
                                ${typeof e.message_count==`number`&&d`
                                    <span class="tc-kv-meta">
                                        ${e.message_count} msg${e.message_count===1?``:`s`}
                                    </span>
                                `}
                                ${e.last_activity&&d`
                                    <span class="tc-kv-meta">${e.last_activity}</span>
                                `}
                            </div>
                            ${e.source_label&&d`
                                <div class="tc-record-body">${e.source_label}</div>
                            `}
                            ${e.summary&&d`
                                <div class="tc-record-body tc-record-summary">
                                    ${e.summary}
                                </div>
                            `}
                        </li>
                    `})}
            </ul>
            <div class="tc-detail-footer">${i.join(` · `)}</div>
        </div>
    `}function Cr(e){if(Q(e))return null;let t;if(typeof e==`string`)t=e;else if(e&&typeof e==`object`)try{t=JSON.stringify(e,null,2)}catch{return null}else if(typeof e==`number`||typeof e==`boolean`)t=String(e);else return null;return d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Echoed</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `}function wr(e){return Q(e)||typeof e!=`number`?null:d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${e}</span>
            </div>
        </div>
    `}function Tr(e){if(typeof e!=`object`||!e||Q(e))return null;let t=typeof e.iso==`string`?e.iso:null,n=typeof e.human==`string`?e.human:null,r=typeof e.timezone==`string`?e.timezone:null,i=typeof e.local_iso==`string`?e.local_iso:null,a=typeof e.local_human==`string`?e.local_human:null,o=typeof e.local_timezone==`string`?e.local_timezone:null,s=typeof e.utc_offset==`string`?e.utc_offset:null;return!t&&!i?null:d`
        ${(t||n)&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">UTC</div>
                <div class="tc-status-row">
                    ${r&&d`<span class="tc-kv-badge">${r}</span>`}
                    ${n&&d`<span class="tc-kv-meta">${n}</span>`}
                </div>
                ${t&&d`<pre class="tc-detail-content">${t}</pre>`}
            </div>
        `}
        ${(i||a)&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Local</div>
                <div class="tc-status-row">
                    ${o&&d`<span class="tc-kv-badge">${o}</span>`}
                    ${s&&d`<span class="tc-kv-meta">${s}</span>`}
                    ${a&&d`<span class="tc-kv-meta">${a}</span>`}
                </div>
                ${i&&d`<pre class="tc-detail-content">${i}</pre>`}
            </div>
        `}
    `}function Er(e){if(typeof e!=`object`||!e||Q(e)||e.ignored!==!0)return null;let t=typeof e.reason==`string`?e.reason:``;return d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Ignored</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">ignored</span>
                ${t&&d`<span class="tc-kv-meta">${t}</span>`}
            </div>
        </div>
    `}function Dr(e,t,n,r){if(t==null)return null;switch(e){case`shell`:case`shell_exec`:return dr(t);case`fs_read`:return fr(t,n);case`fs_write`:case`workspace_write`:case`fs_edit`:return pr(t);case`fs_grep`:return mr(t);case`fs_glob`:return hr(t);case`fs_list`:return gr(t);case`http_get`:return _r(t,n,r);case`invoke_agent`:return vr(t,n,r);case`send_message`:return yr(t);case`read_messages`:case`read_session`:case`read_subagent_session`:return br(t,n,{...r,tool:e});case`list_agents`:return xr(t);case`list_my_sessions`:return Sr(t);case`ignore_message`:return Er(t);case`echo`:return Cr(t);case`math`:return wr(t);case`datetime`:return Tr(t);default:return null}}var Or=500,kr=200;function Ar(e,t){return typeof e==`string`?e.length<=t?e:e.slice(0,t)+`…`:``}var jr={fs_edit:[`old_string`,`new_string`],invoke_agent:[`task`],send_message:[`message`],ignore_message:[`reason`],echo:[`message`,`text`]};function Mr(e,t){if(!t||typeof t!=`object`)return!1;let n=jr[e];if(!n)return!1;for(let e of n){let n=t[e];if(typeof n==`string`&&n.length>kr)return!0}return!1}function Nr(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function Pr(e){if(e==null)return``;if(typeof e==`string`)try{let t=JSON.parse(e);return JSON.stringify(t,null,2)}catch{return e}return JSON.stringify(e,null,2)}function Fr(e){if(e==null)return 0;let t=typeof e==`string`?e:JSON.stringify(e);return new Blob([t]).size}function Ir(e){switch(e){case`shell`:case`shell_exec`:return`$`;case`fs_read`:return`R`;case`fs_write`:return`W`;case`fs_list`:return`L`;case`workspace_write`:return`W`;case`http_get`:return`H`;case`send_message`:return`DM`;case`invoke_agent`:return`IA`;case`read_session`:case`read_subagent_session`:return`RS`;case`list_agents`:return`LA`;case`list_my_sessions`:return`LS`;case`read_messages`:return`RM`;case`ignore_message`:return`IG`;case`math`:return`#`;case`echo`:return`E`;default:return`T`}}function Lr(e,t){if(!t)return null;switch(e){case`shell`:case`shell_exec`:if(t.command)return d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Command</div>
                        <pre class="tc-detail-content tc-code-block">${t.command}</pre>
                    </div>
                `;break;case`fs_read`:{let e=typeof t.offset==`number`?t.offset:null,n=typeof t.limit==`number`?t.limit:null,r=e!=null||n!=null?n==null?`from line ${(e||0)+1}`:`lines ${(e||0)+1}–${(e||0)+n}`:null;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||``}</div>
                    ${r&&d`
                        <div class="tc-status-row">
                            <span class="tc-kv-meta">${r}</span>
                        </div>
                    `}
                </div>
            `}case`fs_write`:return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||``}</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">
                            ${t.mode===`append`?`append`:`overwrite`}
                        </span>
                    </div>
                </div>
                ${t.content&&d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${t.content}</pre>
                    </div>
                `}
            `;case`fs_edit`:{let e=t.replace_all===!0;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||``}</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">
                            ${e?`replace all`:`replace once`}
                        </span>
                    </div>
                </div>
                ${t.old_string&&d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Find</div>
                        <pre class="tc-detail-content tc-code-block">${Ar(t.old_string,kr)}</pre>
                    </div>
                `}
                ${t.new_string&&d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Replace with</div>
                        <pre class="tc-detail-content tc-code-block">${Ar(t.new_string,kr)}</pre>
                    </div>
                `}
            `}case`fs_list`:return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${t.path||`.`}</div>
                </div>
            `;case`fs_grep`:{let e=typeof t.output_mode==`string`?t.output_mode:`files_with_matches`,n=e!==`files_with_matches`;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Pattern</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-mono">${t.pattern||``}</span>
                        ${t.path&&d`
                            <span class="tc-kv-meta">in</span>
                            <span class="tc-kv-mono">${t.path}</span>
                        `}
                    </div>
                    ${(n||t.glob||t.case_insensitive)&&d`
                        <div class="tc-status-row">
                            ${n&&d`<span class="tc-kv-badge">${e}</span>`}
                            ${t.glob&&d`<span class="tc-kv-meta">glob: ${t.glob}</span>`}
                            ${t.case_insensitive&&d`<span class="tc-kv-meta">case-insensitive</span>`}
                        </div>
                    `}
                </div>
            `}case`fs_glob`:return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Pattern</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-mono">${t.pattern||``}</span>
                        ${t.path&&d`
                            <span class="tc-kv-meta">in</span>
                            <span class="tc-kv-mono">${t.path}</span>
                        `}
                    </div>
                </div>
            `;case`invoke_agent`:{let e=t.name||t.subagent_name||``,n=t.background===!0;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Agent</div>
                    <div class="tc-status-row">
                        ${e?d`<span class="tc-kv-badge">${e}</span>`:d`<span class="tc-kv-meta">(ephemeral)</span>`}
                        ${n&&d`<span class="tc-kv-meta">background</span>`}
                    </div>
                </div>
                ${t.task&&d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Task</div>
                        <pre class="tc-detail-content">${Ar(t.task,kr)}</pre>
                    </div>
                `}
            `}case`http_get`:if(t.url)return d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Request</div>
                        <div class="tc-status-row">
                            <span class="tc-kv-badge">GET</span>
                            <span class="tc-kv-mono">${t.url}</span>
                        </div>
                    </div>
                `;break;case`workspace_write`:{let e=t.mode===`append`?`append`:`write`;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Workspace</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${t.file||``}</span>
                        <span class="tc-kv-meta">${e}</span>
                    </div>
                </div>
                ${t.content&&d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${t.content}</pre>
                    </div>
                `}
            `}case`send_message`:return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">To</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${t.to||``}</span>
                    </div>
                </div>
                ${t.message&&d`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Message</div>
                        <pre class="tc-detail-content">${Ar(t.message,kr)}</pre>
                    </div>
                `}
            `;case`read_messages`:{let e=typeof t.last_n==`number`?t.last_n:null;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Filter</div>
                    <div class="tc-status-row">
                        ${t.from&&d`<span class="tc-kv-meta">from</span><span class="tc-kv-badge">${t.from}</span>`}
                        ${e!=null&&d`<span class="tc-kv-meta">last ${e}</span>`}
                    </div>
                </div>
            `}case`read_session`:{let e=typeof t.session_id==`string`&&t.session_id?t.session_id:null,n=typeof t.last_n==`number`?t.last_n:null,r=t.summary_only===!0;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Session</div>
                    <div class="tc-status-row">
                        ${e&&d`<span class="tc-kv-mono">${e}</span>`}
                        ${r&&d`<span class="tc-kv-badge">summary only</span>`}
                        ${n!=null&&d`<span class="tc-kv-meta">last ${n}</span>`}
                    </div>
                </div>
            `}case`read_subagent_session`:{let e=typeof t.last_n==`number`?t.last_n:null,n=t.summary_only===!0;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Subagent</div>
                    <div class="tc-status-row">
                        ${t.name&&d`<span class="tc-kv-badge">${t.name}</span>`}
                        ${n&&d`<span class="tc-kv-badge">summary only</span>`}
                        ${e!=null&&d`<span class="tc-kv-meta">last ${e}</span>`}
                    </div>
                </div>
            `}case`ignore_message`:{let e=typeof t.reason==`string`&&t.reason.length>0?t.reason:null;return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Ignore</div>
                    ${e?d`<pre class="tc-detail-content">${Ar(e,kr)}</pre>`:d`<div class="tc-detail-footer">no reason given</div>`}
                </div>
            `}case`list_my_sessions`:{let e=typeof t.limit==`number`?t.limit:null,n=t.include_current===!0;return e==null&&!n?null:d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Filter</div>
                    <div class="tc-status-row">
                        ${e!=null&&d`<span class="tc-kv-meta">limit ${e}</span>`}
                        ${n&&d`<span class="tc-kv-badge">include current</span>`}
                    </div>
                </div>
            `}case`list_agents`:case`datetime`:return null;case`echo`:return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Message</div>
                    <pre class="tc-detail-content">${Ar(t.message||t.text||``,kr)}</pre>
                </div>
            `;case`math`:{let e=typeof t.operation==`string`?t.operation:``,n=[t.a,t.b,t.n].filter(e=>e!=null);return d`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Expression</div>
                    <div class="tc-status-row">
                        ${e&&d`<span class="tc-kv-badge">${e}</span>`}
                        ${n.length>0&&d`
                            <span class="tc-kv-mono">(${n.join(`, `)})</span>
                        `}
                    </div>
                </div>
            `}}let n=Pr(t);return n?d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters</div>
            <pre class="tc-detail-content">${n}</pre>
        </div>
    `:null}function Rr({params:e}){let t=Pr(e);return t?d`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters (raw)</div>
            <pre class="tc-detail-content">${t}</pre>
        </div>
    `:null}function zr({tool:e,params:t,panelRef:n}){let r=a(!1);i(()=>{Yn(n?.current,`pre.tc-code-block`)});let o=Lr(e,t);return o?Mr(e,t)?d`
        ${r.value?d`<${Rr} params=${t} />`:o}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),r.value=!r.value}}>
                ${r.value?`Hide raw params`:`View raw params`}
            </button>
        </div>
    `:o:null}function Br({result:e,isFail:t,showFull:n,label:r,blockedTarget:i}){let a=Pr(e);if(!a)return null;let o=a.length>Or,s=!n.value&&o?a.slice(0,Or)+`…`:a,c=n.value?` tc-detail-expanded`:``;return d`
        ${i&&d`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Target</div>
                <pre class="tc-detail-content tc-code-block tc-detail-error">${i}</pre>
            </div>
        `}
        <div class="tc-detail-section">
            <div class="tc-detail-label">${r}</div>
            <pre class="tc-detail-content${c} ${t?`tc-detail-error`:``}">${s}</pre>
            ${o&&!t&&d`
                <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),n.value=!n.value}}>
                    ${n.value?`Show less`:`Show more`}
                </button>
            `}
        </div>
    `}function Vr({tool:e,params:t,result:n,isFail:r,isCancelled:o,showFull:s,panelRef:c}){let l=a(!1);if(i(()=>{Yn(c?.current,`pre.tc-code-block`)}),n==null&&!r)return null;let u=r&&typeof n==`object`&&n&&typeof n.target==`string`&&n.target.length>0?n.target:null;if(r)return d`<${Br} result=${n} isFail=${!0}
            showFull=${s} label="Error" blockedTarget=${u} />`;if(o)return d`<${Br} result=${n} isFail=${!1}
            showFull=${s} label="Result (cancelled)" />`;let f=Dr(e,n,t,{showFull:s});return f?d`
        ${l.value?d`<${Br} result=${n} isFail=${!1}
                showFull=${s} label="Result (raw)" />`:f}
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${e=>{e.stopPropagation(),l.value=!l.value}}>
                ${l.value?`Hide raw`:`View raw`}
            </button>
        </div>
    `:d`<${Br} result=${n} isFail=${!1}
            showFull=${s} label="Result" />`}function Hr({tool:e,params:t,status:n,result:r,id:o,sourceAgent:s,durationMs:l}){let u=a(!1),f=a(!1),p=e=>{e.stopPropagation(),u.value=!u.value},m=c(null);i(()=>{u.value&&Yn(m.current,`pre.tc-code-block`)});let h=or(e,t),g=h.length>80?h.slice(0,80)+`…`:h,_=n===`running`,v=n===`fail`,y=n===`done`,b=n===`cancelled`,x=e===`send_message`,S=v?`tc-fail`:y?`tc-done`:b?`tc-cancelled`:`tc-running`,C=u.value?`▼`:`▶`,w=Ir(e),T=Nr(l),E=r==null?0:Fr(r),D=E>=100?sr(E):``;return d`
        <div class="tc-row ${S} ${x?`tc-dm`:``}" role="button" tabindex="0"
             onClick=${p} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),p(e))}}>
            <div class="tc-header">
                <span class="tc-chevron">${C}</span>
                ${_?d`<span class="tc-spinner"></span>`:d`<span class="tc-icon">${w}</span>`}
                <span class="tc-name">${e}</span>
                ${g&&d`<span class="tc-summary">${g}</span>`}
                <span class="tc-spacer"></span>
                ${D&&d`<span class="tc-result-size">${D}</span>`}
                ${T&&d`<span class="tc-duration">${T}</span>`}
                ${v&&d`<span class="tc-status-badge tc-badge-fail">failed</span>`}
                ${b&&d`<span class="tc-status-badge tc-badge-cancelled">cancelled</span>`}
                ${y&&d`<span class="tc-status-icon">\u2713</span>`}
            </div>
            ${u.value&&d`
                <div class="tc-detail" ref=${m}
                     onClick=${e=>e.stopPropagation()}>
                    ${d`<${zr} tool=${e} params=${t}
                        panelRef=${m} />`}
                    ${d`<${Vr} tool=${e} params=${t}
                        result=${r} isFail=${v} isCancelled=${b}
                        showFull=${f} panelRef=${m} />`}
                </div>
            `}
        </div>
    `}function Ur({children:e,count:t}){return t<=1?e:d`
        <div class="tc-group">
            <div class="tc-group-label">${t} tools in parallel</div>
            ${e}
        </div>
    `}function Wr(e,t){return e?e.length<=t?e:e.slice(0,t)+`...`:``}function Gr(e){switch(e){case`system`:return`cd-role-system`;case`user`:return`cd-role-user`;case`assistant`:return`cd-role-assistant`;case`tool`:return`cd-role-tool`;default:return``}}function Kr(e){return e==null?`--`:Number(e).toLocaleString()}function qr({msg:e,index:t}){let n=a(!1),r=e.role||`unknown`,i=e.content||``,o=e.tool_calls&&e.tool_calls.length>0,s=!!e.tool_call_id,c=Wr(i,120),l=`[${t}] ${r}`;if(s&&(l+=` (tool_result)`),o){let t=e.tool_calls.map(e=>e.function?.name||`?`).join(`, `);l+=` -> ${t}`}return d`
        <div class="cd-msg" role="button" tabindex="0"
             onClick=${e=>{e.stopPropagation(),n.value=!n.value}}
             onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),e.stopPropagation(),n.value=!n.value)}}>
            <div class="cd-msg-header">
                <span class="cd-msg-chevron">${n.value?`▼`:`▶`}</span>
                <span class="cd-msg-role ${Gr(r)}">${r}</span>
                ${!n.value&&c&&d`<span class="cd-msg-preview">${c}</span>`}
            </div>
            ${n.value&&d`
                <div class="cd-msg-body" onClick=${e=>e.stopPropagation()}>
                    ${i&&d`<pre class="cd-msg-content">${i}</pre>`}
                    ${o&&d`
                        <div class="cd-msg-tools">
                            <div class="cd-section-label">Tool calls:</div>
                            ${e.tool_calls.map(e=>d`
                                <pre class="cd-msg-content">${e.function?.name||`?`}(${e.function?.arguments||``})</pre>
                            `)}
                        </div>
                    `}
                </div>
            `}
        </div>
    `}function Jr({messages:e,toolNames:t,totalTokens:n,systemTokens:r,historyMessageCount:i,agentName:o,agentId:s}){let c=a(!1),l=e=>{e.stopPropagation(),c.value=!c.value},u=Array.isArray(e)?e.length:0,f=o?`Context sent to LLM (${o})`:`Context sent to LLM`;return d`
        <div class="cd-row" role="button" tabindex="0"
             onClick=${l} onKeyDown=${e=>{(e.key===`Enter`||e.key===` `)&&(e.preventDefault(),l(e))}}>
            <div class="cd-header">
                <span class="cd-chevron">${c.value?`▼`:`▶`}</span>
                <span class="cd-icon">CTX</span>
                <span class="cd-title">${f}</span>
                <span class="cd-stats">
                    ${Kr(n)} tokens | ${u} messages | ${(t||[]).length} tools
                </span>
            </div>
            ${c.value&&d`
                <div class="cd-detail" onClick=${e=>e.stopPropagation()}>
                    <!-- Token breakdown -->
                    <div class="cd-section">
                        <div class="cd-section-label">Token breakdown</div>
                        <div class="cd-token-grid">
                            <span class="cd-token-label">System prompt:</span>
                            <span class="cd-token-value">${Kr(r)}</span>
                            <span class="cd-token-label">History messages:</span>
                            <span class="cd-token-value">${i}</span>
                            <span class="cd-token-label">Total estimated:</span>
                            <span class="cd-token-value cd-token-total">${Kr(n)}</span>
                        </div>
                    </div>

                    <!-- Tools available -->
                    <div class="cd-section">
                        <div class="cd-section-label">Tools available (${(t||[]).length})</div>
                        <div class="cd-tool-list">
                            ${(t||[]).map(e=>d`<span class="cd-tool-tag">${e}</span>`)}
                        </div>
                    </div>

                    <!-- Messages -->
                    <div class="cd-section">
                        <div class="cd-section-label">Messages (${u})</div>
                        <div class="cd-messages">
                            ${(e||[]).map((e,t)=>d`
                                <${qr} key=${t} msg=${e} index=${t} />
                            `)}
                        </div>
                    </div>
                </div>
            `}
        </div>
    `}async function Yr(e,t){try{await g(`/approvals/${e}`,{decision:t})}catch(e){throw console.error(`[resolveApproval] failed:`,e),e}}function Xr({approvalId:e,tool:t,params:n}){let r=a(!1),i=async()=>{if(!r.value){r.value=!0;try{await Yr(e,`approve`)}catch{r.value=!1}}},o=async()=>{if(!r.value){r.value=!0;try{await Yr(e,`deny`)}catch{r.value=!1}}},s=r.value;return d`
        <div class="approval-card">
            <h3>\u26a0 Approval required \u2014 ${t}</h3>
            <pre>${JSON.stringify(n,null,2)}</pre>
            <div class="approval-btns">
                <button class="btn btn-approve" onClick=${i} disabled=${s}>
                    ${s?`Submitting...`:`Approve`}
                </button>
                <button class="btn btn-deny" onClick=${o} disabled=${s}>
                    ${s?`Submitting...`:`Deny`}
                </button>
            </div>
        </div>
    `}function Zr(e){return typeof e==`string`&&e.endsWith(`...`)}function Qr({runId:e,summary:t,truncated:n}={}){return e?typeof n==`boolean`?n:Zr(t):!1}function $r({jobSessionUuid:e}={}){return typeof e==`string`&&e.length>0?e:null}function ei(e,t){return typeof t==`string`&&t.length>0?t:e||``}var ti=150;function ni(e){if(!e)return``;try{return new Date(e).toLocaleTimeString(void 0,{hour:`2-digit`,minute:`2-digit`})}catch{return``}}function ri(e){switch(e){case`success`:return`Completed`;case`error`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Finished`}}function ii(e){switch(e){case`success`:return`✓`;case`error`:return`✗`;case`cancelled`:return`–`;default:return`•`}}function ai({jobName:e,status:t,summary:n,ts:r,runId:o,truncated:s,jobSessionUuid:c,jobSessionId:l}){let f=a(!1),p=a(null),m=a(!1),h=n&&n.length>ti,g=!h||f.value;i(()=>{if(!f.value||p.value!==null||m.value||!Qr({runId:o,summary:n,truncated:s}))return;m.value=!0;let e=!1;return Xe(o).then(t=>{e||(p.value=ei(n,t&&t.response))}).catch(()=>{e||(p.value=n||``)}).finally(()=>{e||(m.value=!1)}),()=>{e=!0}},[f.value,o,n,s]);let _=p.value==null?n:p.value,v=`job-card--${t||`success`}`,y=ni(r),b=ii(t),x=ri(t),S=()=>{f.value=!f.value},C=$r({jobSessionUuid:c}),w=e=>{e.stopPropagation(),C&&Jt(C,{logPrefix:`job-card`})},T=_?u(_):``;return d`
        <div class="job-card ${v}">
            <div class="job-card-header">
                <span class="job-card-icon">${b}</span>
                <span class="job-card-badge">${x}</span>
                <span class="job-card-label">Scheduled Job</span>
                ${y&&d`<span class="job-card-time">${y}</span>`}
            </div>
            <div class="job-card-name">${e||`unnamed job`}</div>
            ${n&&d`
                <div class="job-card-body">
                    ${g?d`<div class="job-card-summary markdown-body"
                                     dangerouslySetInnerHTML=${{__html:T}} />`:d`<div class="job-card-summary-truncated">
                                ${n.slice(0,ti)}...
                            </div>`}
                </div>
            `}
            ${(h||C)&&d`
                <div class="job-card-actions">
                    ${h&&d`
                        <button class="job-card-toggle" onClick=${S}>
                            ${f.value?`Show less`:`Show more`}
                        </button>
                    `}
                    ${C&&d`
                        <button class="job-card-goto" onClick=${w}>
                            Go to job session →
                        </button>
                    `}
                </div>
            `}
        </div>
    `}var oi=200;function si(e){if(e==null)return``;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m `+n+`s`}function ci(e){switch(e){case`done`:return`Completed`;case`fail`:return`Failed`;case`cancelled`:return`Cancelled`;default:return`Completed`}}function li(e){switch(e){case`done`:return`✓`;case`fail`:return`✗`;case`cancelled`:return`–`;default:return`✓`}}function ui({name:e,task:t,status:n,toolCount:r,durationMs:i,sessionId:o,summary:s}){let c=a(!1),l=`sa-card--${n||`done`}`,u=li(n),f=ci(n),p=si(i),m=s&&s.length>oi,h=!c.value&&m?s.slice(0,oi)+`…`:s;return d`
        <div class="sa-card ${l}">
            <div class="sa-card-header">
                <span class="sa-card-icon">${u}</span>
                <span class="sa-card-badge">${f}</span>
                <span class="sa-card-label">Subagent</span>
                ${p&&d`<span class="sa-card-meta">${p}</span>`}
                ${r>0&&d`<span class="sa-card-meta">${r} tool${r===1?``:`s`}</span>`}
            </div>
            <div class="sa-card-name">${e||`subagent`}</div>
            ${t&&d`<div class="sa-card-task">${t}</div>`}
            ${s&&d`
                <div class="sa-card-body">
                    <div class="sa-card-summary">${h}</div>
                    ${m&&d`
                        <button class="sa-card-toggle" onClick=${()=>{c.value=!c.value}}>
                            ${c.value?`Show less`:`Show more`}
                        </button>
                    `}
                </div>
            `}
            ${o&&d`
                <div class="sa-card-actions">
                    <button class="sa-card-view-btn" onClick=${e=>{e.stopPropagation(),o&&fe(o)}}>
                        View session \u2192
                    </button>
                </div>
            `}
        </div>
    `}function di(e){let t=O.value.filter((t,n)=>n!==e);O.value=t,cn(E.value,t)}function fi(){let e=O.value;return e.length===0?null:d`
        <div id="message-queue">
            ${e.map((e,t)=>d`
                <div class="queued-msg">
                    <span class="queued-msg-label">queued</span>
                    <span class="queued-msg-text">${e.text}</span>
                    <button class="queued-msg-remove" title="Remove from queue"
                            onClick=${()=>di(t)}>\u00d7</button>
                </div>
            `)}
        </div>
    `}async function pi(e,t){let n=t?.sessionId||E.value,r=V.value;if(!r)return n?ze(e=>[...e,{id:A(),type:`error`,text:`Select an agent before sending a message.`}],n):console.warn(`[startRun] rejected: no agent or session selected`),!1;if(!n)return console.warn(`[startRun] rejected: no session selected`),!1;if(k(n).length>0)return!1;let i=new Date().toISOString(),a={id:A(),type:`user`,role:`user`,text:e,ts:i};j(n,e,a,[{id:A(),type:`thinking`,pending:!0}]);try{let i=await Je({session_id:n,agent_id:r,input:{type:`text`,text:e}});n&&i?.run_id&&(x(n,a.id,i.run_id),!t?.queued&&!U.value&&O.value.length>0&&await mi(O.value[0],n))}catch(e){ee(n,{messageId:a.id}),ze(t=>[...t.filter(e=>e.type!==`thinking`),{id:A(),type:`error`,text:`Failed to start run: ${e.error?.message||e.message||e.status||`unknown error`}`}],n),console.error(`[startRun] failed:`,e)}return!0}async function mi(e,t){let n=O.value;if(n[0]!==e)return!1;let r=await pi(e.text,{sessionId:t,queued:!0}),i=O.value,a=dn(i,e,r);if(a===i){let i=dn(n,e,r);return i===n?!1:(cn(t,i),!0)}return O.value=a,cn(t,a),!U.value&&a.length>0&&await mi(a[0],t),!0}function hi(e){let t=e.current.value.trim();if(!t||!E.value||!V.value)return;let n=E.value;if(!(!U.value&&k(n).length>0)){if(e.current.value=``,e.current.style.height=`auto`,on(n),U.value){let r=[...O.value,{text:t}];O.value=r,cn(n,r),e.current.focus();return}pi(t)}}async function gi(){if(U.value)try{await Ze(U.value)}catch{}}function _i(e){e.style.height=`auto`,e.style.height=Math.min(e.scrollHeight,150)+`px`}function vi(){let e=c(null),t=H.value.length>0,n=!!E.value,r=!!V.value,a=t&&r&&n,o=!!U.value,s=r?`Send a message...`:`Select an agent to send a message`,l=E.value;return i(()=>{let t=e.current;t&&(t.value=rn(l),_i(t));let n=sn(l),r=fn({restoredQueue:n,activeRunId:U.value,activeAgentId:V.value});n.length>0&&(O.value=n),r.drain&&mi(r.head,l).catch(e=>{console.error(`[queue] mount drain failed:`,e)})},[l]),d`
        <div id="input-area">
            <div class="input-container">
                <textarea id="prompt" ref=${e} rows="1"
                          placeholder=${s}
                          aria-label="Message input"
                          disabled=${!a}
                          onKeyDown=${t=>{t.key===`Enter`&&!t.shiftKey&&(t.preventDefault(),hi(e))}}
                          onInput=${()=>{let t=e.current;t&&(_i(t),an(l,t.value))}}></textarea>
                ${o?d`<button id="cancel-run" title="Stop run" aria-label="Stop run"
                                   onClick=${gi}><${It} /></button>`:d`<button id="send" disabled=${!a}
                                   title="Send (Enter)" aria-label="Send message"
                                   onClick=${()=>hi(e)}><${Ft} /></button>`}
            </div>
        </div>
    `}var yi=()=>f(`/agents`),bi=e=>g(`/agents`,e),xi=(e,t)=>_(`/agents/${e}`,t),Si=e=>ne(`/agents/${e}`),Ci=e=>g(`/agents/${e}/default`),wi={"claude-opus-4-7":{name:`Claude Opus 4.7`,provider:`anthropic`},"claude-opus-4-6":{name:`Claude Opus 4.6`,provider:`anthropic`},"claude-sonnet-4-6":{name:`Claude Sonnet 4.6`,provider:`anthropic`},"claude-sonnet-4-5":{name:`Claude Sonnet 4.5`,provider:`anthropic`},"claude-haiku-4-5":{name:`Claude Haiku 4.5`,provider:`anthropic`},"gpt-5.4":{name:`GPT-5.4`,provider:`openai`},"gpt-5.4-mini":{name:`GPT-5.4 mini`,provider:`openai`},"gpt-5.4-nano":{name:`GPT-5.4 nano`,provider:`openai`},"gpt-4.1":{name:`GPT-4.1`,provider:`openai`},"gpt-4.1-mini":{name:`GPT-4.1 mini`,provider:`openai`},"gpt-4.1-nano":{name:`GPT-4.1 nano`,provider:`openai`},"gpt-4o":{name:`GPT-4o`,provider:`openai`},"gpt-4o-mini":{name:`GPT-4o mini`,provider:`openai`},"o4-mini":{name:`o4-mini`,provider:`openai`},o3:{name:`o3`,provider:`openai`},"o3-mini":{name:`o3-mini`,provider:`openai`},"grok-4.20":{name:`Grok 4.20`,provider:`xai`},"grok-4-fast":{name:`Grok 4 Fast`,provider:`xai`},"grok-3":{name:`Grok 3`,provider:`xai`},"grok-3-mini":{name:`Grok 3 mini`,provider:`xai`},"deepseek-chat":{name:`DeepSeek Chat (V3)`,provider:`deepseek`},"deepseek-reasoner":{name:`DeepSeek Reasoner (R1)`,provider:`deepseek`},"mistral-large-latest":{name:`Mistral Large`,provider:`mistral`},"mistral-medium-latest":{name:`Mistral Medium`,provider:`mistral`},"mistral-small-latest":{name:`Mistral Small`,provider:`mistral`},"codestral-latest":{name:`Codestral`,provider:`mistral`},"ministral-8b-latest":{name:`Ministral 8B`,provider:`mistral`},"open-mistral-nemo":{name:`Mistral Nemo`,provider:`mistral`},"llama-3.3-70b-versatile":{name:`Llama 3.3 70B`,provider:`groq`},"llama-3.1-8b-instant":{name:`Llama 3.1 8B (Instant)`,provider:`groq`},"deepseek-r1-distill-llama-70b":{name:`DeepSeek R1 Distill (70B)`,provider:`groq`},"qwen-2.5-32b":{name:`Qwen 2.5 32B`,provider:`groq`},"qwen2.5-coder:32b":{name:`Qwen 2.5 Coder 32B`,provider:`ollama`},"deepseek-r1:7b":{name:`DeepSeek R1 7B`,provider:`ollama`},"llama3.3:70b":{name:`Llama 3.3 70B`,provider:`ollama`},"deepseek/deepseek-r1":{name:`DeepSeek R1`,provider:`openrouter`},"deepseek/deepseek-chat-v3-0324":{name:`DeepSeek Chat v3`,provider:`openrouter`},"z-ai/glm-5.2":{name:`GLM 5.2`,provider:`openrouter`},"z-ai/glm-5.1":{name:`GLM 5.1`,provider:`openrouter`},"minimax/minimax-m2.7":{name:`MiniMax M2.7`,provider:`openrouter`},"xiaomi/mimo-v2-pro":{name:`MiMo v2-pro`,provider:`openrouter`},"moonshotai/kimi-k2.6":{name:`Kimi K2.6`,provider:`openrouter`},"google/gemma-4-31b-it":{name:`Gemma 4 31B`,provider:`openrouter`}},Ti=`claude-opus-4-7,claude-sonnet-4-6,claude-haiku-4-5,claude-opus-4-6,gpt-5.4,gpt-5.4-mini,gpt-5.4-nano,gpt-4.1,gpt-4.1-mini,gpt-4o,gpt-4o-mini,o4-mini,o3,grok-4.20,grok-4-fast,grok-3-mini,deepseek-chat,deepseek-reasoner,mistral-large-latest,mistral-small-latest,codestral-latest,llama-3.3-70b-versatile,llama-3.1-8b-instant,deepseek-r1-distill-llama-70b,qwen2.5-coder:32b,deepseek-r1:7b,llama3.3:70b,z-ai/glm-5.2,deepseek/deepseek-r1,deepseek/deepseek-chat-v3-0324,z-ai/glm-5.1,minimax/minimax-m2.7,xiaomi/mimo-v2-pro,moonshotai/kimi-k2.6,google/gemma-4-31b-it`.split(`,`);function Ei(e){if(!e)return``;let t=wi[e];return t?t.name:e}function Di(e){if(!e)return`unknown`;let t=wi[e];return t?t.provider:e.includes(`/`)?`openrouter`:e.includes(`:`)?`ollama`:e.startsWith(`claude`)?`anthropic`:e.startsWith(`gpt`)||/^o\d/.test(e)?`openai`:e.startsWith(`grok`)?`xai`:e.startsWith(`deepseek-`)?`deepseek`:e.startsWith(`mistral-`)||e.startsWith(`codestral-`)||e.startsWith(`ministral-`)||e.startsWith(`open-mistral-`)||e.startsWith(`open-mixtral-`)?`mistral`:e.startsWith(`llama-`)?`groq`:e.startsWith(`gemini-`)?`google`:`unknown`}var Oi={anthropic:`Anthropic`,openai:`OpenAI`,openrouter:`OpenRouter`,xai:`xAI`,deepseek:`DeepSeek`,mistral:`Mistral`,groq:`Groq`,ollama:`Ollama`,google:`Google`,unknown:`Custom`};function ki(e){return e?Oi[e]?Oi[e]:Oi[Di(e)]:Oi.unknown}function Ai({modelId:e,provider:t}){let n=t||Di(e),r=Oi[n]||Oi.unknown;return d`
        <span class="model-provider-badge model-provider-badge--${n}"
              title=${`Provider: ${r}`}>${r}</span>
    `}function ji({value:e,defaultValue:t,showBadge:n=!0}){let r=e&&e.trim?e.trim():e,i=!!r&&r!==t,a=r||t;if(!a)return d`<span class="model-display model-display--muted">unknown</span>`;let o=Ei(a),s=o===a?a:`${o} (${a})`;return i?d`
            <span class="model-display" title=${s}>
                <span class="model-override-pill" title="Per-run override">override</span>
                <span class="model-name">${o}</span>
                ${n&&d`<${Ai} modelId=${a} />`}
            </span>
        `:d`
        <span class="model-display model-display--default" title=${s}>
            <span class="model-default-label">Default</span>
            <span class="model-name">${o}</span>
            ${n&&d`<${Ai} modelId=${a} />`}
        </span>
    `}function Mi({value:e,defaultValue:t}){let n=e&&e.trim?e.trim():e,r=!!n&&n!==t,i=n||t;return i?r?d`
            <span class="model-display">
                <span class="model-override-pill" title="Per-run override">override</span>
                <${Ai} provider=${i} />
            </span>
        `:d`
        <span class="model-display model-display--default">
            <span class="model-default-label">Default</span>
            <${Ai} provider=${i} />
        </span>
    `:d`<span class="model-display model-display--muted">unknown</span>`}function Ni(e){return e==null?``:String(e).toLowerCase().replace(/\s+/g,`-`).replace(/[^a-z0-9-]/g,``).replace(/-+/g,`-`).replace(/^-+|-+$/g,``)}var Pi=[`default`,`dm`,`workspace`],Fi=/^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32})$/,Ii=64;function Li(e){return typeof e!=`string`||e.length===0?null:e.length>Ii?{code:`AGENT_NAME_TOO_LONG`,message:`Agent name is too long (max ${Ii} characters after normalization)`}:Pi.includes(e)?{code:`AGENT_NAME_RESERVED`,message:`'${e}' is a reserved name`}:Fi.test(e)?{code:`AGENT_NAME_LOOKS_LIKE_UUID`,message:`'${e}' looks like a UUID (conflicts with ID-based lookup)`}:null}function Ri(e){return e===`Enter`||e===` `}function zi(e){return!e||typeof e.key!=`string`||e.defaultPrevented?!1:Ri(e.key)}function Bi(e){return!(!e||e.defaultPrevented)}function Vi(e,t){let n=!!e,r=!!t;return n===r?{}:{debug_mode:r}}var Hi=[`minimal`,`low`,`medium`,`high`];function Ui(e){return e==null?`inherit`:e===0?`disable`:`custom`}function Wi(e){return e==null||e===``?`inherit`:`custom`}function Gi({label:e,hint:t,agentValue:n,mode:r,draft:i}){return d`
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
                ${r.value===`custom`&&d`
                    <input class="settings-input agent-tristate-value"
                           type="number" min="0" step="1024"
                           placeholder="tokens"
                           value=${i.value}
                           onInput=${e=>{i.value=e.target.value}} />
                `}
            </div>
            ${t&&d`<span class="settings-hint">${t}</span>`}
        </div>
    `}function Ki({label:e,hint:t,mode:n,value:r}){return d`
        <div class="settings-row">
            <label class="settings-label">${e}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${n.value}
                        onChange=${e=>{n.value=e.target.value,n.value!==`custom`&&(r.value=``)}}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="custom">Custom</option>
                </select>
                ${n.value===`custom`&&d`
                    <select class="settings-select agent-tristate-value"
                            value=${r.value}
                            onChange=${e=>{r.value=e.target.value}}>
                        <option value="">choose...</option>
                        ${Hi.map(e=>d`<option value=${e} key=${e}>${e}</option>`)}
                    </select>
                `}
            </div>
            ${t&&d`<span class="settings-hint">${t}</span>`}
        </div>
    `}async function qi(){try{let e=await yi();Me(e.agents||e||[])}catch(e){console.error(`[agents] fetch failed:`,e)}}function Ji({agent:e,onClose:t}){let n=a(e.description||``),r=a(e.model||``),i=a(e.posture||``),o=a(e.provider||``),s=a(`keep`),c=a(``),l=a(Ui(e.thinking_budget_tokens)),u=a(e.thinking_budget_tokens&&e.thinking_budget_tokens>0?String(e.thinking_budget_tokens):``),f=a(Wi(e.reasoning_effort)),p=a(e.reasoning_effort||``),m=a(Ui(e.gemini_thinking_budget)),h=a(e.gemini_thinking_budget&&e.gemini_thinking_budget>0?String(e.gemini_thinking_budget):``),g=a(e.summary_provider||``),_=a(e.summary_model||``),v=a(!!e.debug_mode),y=a(!1),b=a(``),x=W.value.model||``,S=W.value.provider||``,C=W.value.llm_providers||[],w=()=>{let t={};n.value!==(e.description||``)&&(t.description=n.value),(r.value||``)!==(e.model||``)&&(t.model=r.value||``),(i.value||``)!==(e.posture||``)&&(t.posture=i.value||``),(o.value||``)!==(e.provider||``)&&(t.provider=o.value||``),s.value===`set`&&c.value.trim()?t.telegram_token=c.value.trim():s.value===`remove`&&(t.telegram_token=``);let a=e.thinking_budget_tokens;if(l.value===`inherit`)a!=null&&(t.clear_thinking_budget_tokens=!0);else if(l.value===`disable`)a!==0&&(t.thinking_budget_tokens=0);else if(l.value===`custom`){let e=parseInt(u.value,10);!isNaN(e)&&e>=0&&e!==a&&(t.thinking_budget_tokens=e)}let d=e.reasoning_effort||null;f.value===`inherit`?d!=null&&(t.clear_reasoning_effort=!0):f.value===`custom`&&p.value&&p.value!==d&&(t.reasoning_effort=p.value);let y=e.gemini_thinking_budget;if(m.value===`inherit`)y!=null&&(t.clear_gemini_thinking_budget=!0);else if(m.value===`disable`)y!==0&&(t.gemini_thinking_budget=0);else if(m.value===`custom`){let e=parseInt(h.value,10);!isNaN(e)&&e>=0&&e!==y&&(t.gemini_thinking_budget=e)}let b=e.summary_provider||``,x=(g.value||``).trim();x!==b&&(x===``?t.clear_summary_provider=!0:t.summary_provider=x);let S=e.summary_model||``,C=(_.value||``).trim();return C!==S&&(C===``?t.clear_summary_model=!0:t.summary_model=C),Object.assign(t,Vi(e.debug_mode,v.value)),t},T=async()=>{y.value=!0,b.value=``;try{let n=w();if(Object.keys(n).length===0){t();return}await xi(e.id,n),await qi(),t()}catch(e){b.value=e.error?.message||e.message||`Save failed`}finally{y.value=!1}},E=e=>{e.target===e.currentTarget&&t()},D=!!e.has_telegram;return d`
        <div class="settings-overlay open" onClick=${E}>
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
                        ${Ti.map(e=>d`<option value=${e} />`)}
                    </datalist>
                    <span class="settings-effective">
                        Effective: <${ji} value=${r.value.trim()} defaultValue=${x} />
                    </span>
                    <span class="settings-hint">Leave empty to use server default.</span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Provider</label>
                    <select class="settings-select"
                            value=${o.value}
                            onChange=${e=>{o.value=e.target.value}}>
                        <option value="">Default (${ki(S||`openai`)})</option>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="openrouter">OpenRouter</option>
                    </select>
                    <span class="settings-effective">
                        Effective: <${Mi} value=${o.value} defaultValue=${S||`openai`} />
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

                <span class="settings-hint">
                    Provider-specific. Each row applies only when this agent's effective provider matches —
                    Anthropic / OpenAI / Gemini knobs are silently ignored on other providers. Explicit values
                    take effect on the next run.
                </span>

                <${Gi}
                    label="Anthropic thinking budget"
                    hint="Inherit = use server default. Disable = Some(0) (force off for this agent). Custom = override with N tokens."
                    agentValue=${e.thinking_budget_tokens}
                    mode=${l}
                    draft=${u} />

                <${Ki}
                    label="OpenAI reasoning effort"
                    hint="Inherit = use server default. Custom picks an effort level for this agent."
                    mode=${f}
                    value=${p} />

                <${Gi}
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
                        ${(C.length>0?C:[`openai`,`anthropic`,`openrouter`,`gemini`]).map(e=>{let t=ki(e);return d`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
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
                            ${D?`configured (token hidden)`:`not configured`}
                        </span>
                        <select class="settings-select agent-tristate-mode"
                                value=${s.value}
                                onChange=${e=>{s.value=e.target.value,e.target.value!==`set`&&(c.value=``)}}>
                            <option value="keep">Keep</option>
                            <option value="set">${D?`Replace`:`Set`}</option>
                            ${D&&d`<option value="remove">Remove</option>`}
                        </select>
                    </div>
                    ${s.value===`set`&&d`
                        <input class="settings-input" type="password"
                               autocomplete="off"
                               placeholder="paste bot token..."
                               value=${c.value}
                               onInput=${e=>{c.value=e.target.value}} />
                    `}
                    <span class="settings-hint">
                        ${D?`A token is set but is never displayed. Replace overwrites it; Remove clears it.`:`Set a bot token to enable a dedicated Telegram polling loop for this agent.`}
                    </span>
                    <span class="settings-hint">
                        Token changes only take effect after the daemon restarts. Tracked in #821.
                    </span>
                </div>

                ${b.value&&d`<div class="inline-error">${b.value}</div>`}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${t}>Cancel</button>
                    <button class="settings-save" onClick=${T} disabled=${y.value}>
                        ${y.value?`...`:`Save`}
                    </button>
                </div>
            </div>
        </div>
    `}function Yi({agent:e,isActive:t,onEdit:n}){let r=a(``),i=a(!1),o=a(null),s=W.value.model||``,c=W.value.provider||``;return d`
        <div class="agent-card ${t?`active`:``}"
             role="option"
             tabindex="0"
             aria-label=${`Select agent `+e.name}
             aria-selected=${t?`true`:`false`}
             onClick=${t=>{Bi(t)&&Et(e.id)}}
             onKeyDown=${t=>{zi(t)&&(t.preventDefault(),Et(e.id))}}>
            <div class="agent-card-header">
                <span class="agent-card-name">${e.name}</span>
                ${e.is_default&&d`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta agent-card-meta--model">
                <span class="agent-card-meta-label">model:</span>
                <${ji} value=${e.model} defaultValue=${s} />
            </div>
            ${e.provider&&d`
                <div class="agent-card-meta agent-card-meta--provider">
                    <span class="agent-card-meta-label">provider:</span>
                    <${Mi} value=${e.provider} defaultValue=${c} />
                </div>
            `}
            ${e.posture&&d`
                <div class="agent-card-meta agent-card-meta--posture">
                    <span class="agent-card-meta-label">posture:</span>
                    <span>${e.posture}</span>
                </div>
            `}
            ${r.value&&d`<div class="agent-error">${r.value}</div>`}
            <div class="agent-card-actions">
                <button class="agent-card-btn" onClick=${t=>{t&&t.stopPropagation(),n(e)}}>Edit</button>
                ${!e.is_default&&d`
                    <button class="agent-card-btn" onClick=${async t=>{t&&t.stopPropagation();try{await Ci(e.id),await qi()}catch(e){r.value=e.error?.message||e.message||`Failed`}}}>Set Default</button>
                `}
                ${i.value?d`
                        <button class="agent-card-btn" style="color:var(--error); font-weight:600;" onClick=${async t=>{t&&t.stopPropagation(),o.value&&=(clearTimeout(o.value),null),i.value=!1;try{if(await Si(e.id),await qi(),e.id===V.value){let e=H.value.find(e=>e.is_default)||H.value[0]||null;e?Et(e.id):(V.value=null,S.value=null)}}catch(e){r.value=e.error?.message||e.message||`Delete failed`}}}>Confirm?</button>
                        <button class="agent-card-btn" onClick=${e=>{e&&e.stopPropagation(),o.value&&=(clearTimeout(o.value),null),i.value=!1}}>Cancel</button>
                    `:d`<button class="agent-card-btn" style="color:var(--error);" onClick=${e=>{e&&e.stopPropagation(),i.value=!0,o.value=setTimeout(()=>{i.value=!1},3e3)}}>Delete</button>`}
            </div>
        </div>
    `}function Xi(){let e=a(``),t=a(``),n=a(!1),r=a(null);i(()=>{Z.value===`agents`&&qi()},[Z.value]);let o=Ni(e.value),s=(e.value||``).trim(),c=s!==``&&o!==s,l=async()=>{let r=Ni(e.value);if(!r){(e.value||``).trim()===``?t.value=`Agent name is required`:t.value=`Agent name must contain at least one letter or digit`;return}let i=Li(r);if(i){t.value=i.message;return}t.value=``,n.value=!0;try{let t=await bi({name:r});t.id||console.warn(`[agents] POST /agents returned no id for agent:`,r,t),e.value=``,await qi()}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}};return d`
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
            ${c&&d`
                <div class="agent-create-preview">
                    Will be saved as: <code>${o}</code>
                </div>
            `}

            ${t.value&&d`<div class="agent-error">${t.value}</div>`}

            <div class="agent-list" role="listbox" aria-label="Agents">
                ${H.value.length===0?d`<div class="empty-state">No agents</div>`:H.value.map(e=>d`
                        <${Yi} key=${e.id} agent=${e}
                                      isActive=${e.id===V.value}
                                      onEdit=${e=>{r.value=e}} />
                    `)}
            </div>

            ${r.value&&d`
                <${Ji}
                    agent=${r.value}
                    onClose=${()=>{r.value=null}} />
            `}
        </div>
    `}var Zi=e=>f(`/agents/${e}/workspace`),Qi=(e,t,n)=>_(`/agents/${e}/workspace/${t}`,{content:n}),$i=e=>g(`/agents/${e}/workspace/open`,{}),ea=[`personality`,`goals`,`memories`,`user`];async function ta(){if(!V.value){G.value=null;return}try{let e=await Zi(V.value);G.value=e.files||e}catch(e){e.status===404||e.error?.code===`NOT_FOUND`?G.value=`unavailable`:G.value=`error`}}function na({agentId:e,doOpen:t}){let n=a(!1),r=a(null),i=async()=>{if(!(n.value||!e)){n.value=!0,r.value=null;try{await t(e),r.value={kind:`ok`,text:`Opened`},setTimeout(()=>{r.value?.kind===`ok`&&(r.value=null)},2e3)}catch(e){let t=e?.error?.code,n=e?.error?.message||e?.message||`Failed to open workspace`,i=n;t===`NOT_CONFIGURED`?i=`Workspace dir not configured`:t===`WORKSPACE_PATH_MISSING`?i=`Workspace path is missing on disk`:t===`LAUNCHER_FAILED`&&(i=`Failed to launch file explorer`),r.value={kind:`err`,text:i,full:n}}finally{n.value=!1}}};return d`
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
            ${r.value&&d`
                <span class="ws-flash ${r.value.kind===`ok`?`ok`:`err`}"
                      title=${r.value.full||``}
                      role=${r.value.kind===`err`?`alert`:`status`}>
                    ${r.value.text}
                </span>
            `}
        </div>
    `}function ra({agentId:e,filename:t,content:n}){let r=a(n||``),o=a(``),s=a(!1);return i(()=>{r.value=n||``},[n]),d`
        <div class="ws-file">
            <div class="ws-file-label">${t}</div>
            <textarea class="ws-textarea"
                      rows="6"
                      value=${r.value}
                      onInput=${e=>{r.value=e.target.value}}></textarea>
            <div style="display:flex; align-items:center; gap:var(--space-2);">
                <button class="ws-save" onClick=${async()=>{if(!s.value){s.value=!0,o.value=``;try{await Qi(e,t,r.value),o.value=`Saved`,setTimeout(()=>{o.value=``},2e3),await ta()}catch(e){o.value=`Error: `+(e.error?.message||e.message||`save failed`)}finally{s.value=!1}}}} disabled=${s.value}>
                    ${s.value?`Saving...`:`Save`}
                </button>
                ${o.value&&d`
                    <span class="ws-flash ${o.value.startsWith(`Error`)?`err`:`ok`}">
                        ${o.value}
                    </span>
                `}
            </div>
        </div>
    `}function ia(){return i(()=>{Z.value===`workspace`&&ta()},[Z.value,V.value]),V.value?G.value===null?d`<div class="loading-state">Loading...</div>`:G.value===`unavailable`?d`<div class="ws-notice">Workspace not configured for this agent</div>`:G.value===`error`?d`<div class="ws-notice" style="color:var(--error);">Failed to load workspace</div>`:d`
        <div>
            <${na}
                agentId=${V.value}
                doOpen=${$i} />
            ${ea.map(e=>d`
                <${ra}
                    key=${e}
                    agentId=${V.value}
                    filename=${e}
                    content=${G.value[e+`.md`]||G.value[e]||``} />
            `)}
        </div>
    `:d`<div class="ws-notice">No agent selected</div>`}var aa=z.jobs;function oa(){return z.getJobMutationGeneration()}function sa(e,t){z.replaceJobs(e,t)}function ca(e){z.createOptimisticJob(e)}function la(e,t){z.confirmOptimisticJobCreate(e,t)}function ua(e){z.rollbackOptimisticJobCreate(e)}function da(e){z.cancelOptimisticJob(e)}function fa(e,t){z.confirmOptimisticJobCancel(e,t)}function pa(e){z.rollbackOptimisticJobCancel(e)}var ma=()=>f(`/jobs`),ha=e=>g(`/jobs`,e),ga=e=>ne(`/jobs/${e}`),_a=0,va=[{label:`1m`,cron:`* * * * *`,desc:`Every minute`},{label:`5m`,cron:`*/5 * * * *`,desc:`Every 5 minutes`},{label:`15m`,cron:`*/15 * * * *`,desc:`Every 15 minutes`},{label:`30m`,cron:`*/30 * * * *`,desc:`Every 30 minutes`},{label:`1h`,cron:`0 * * * *`,desc:`Every hour`},{label:`6h`,cron:`0 */6 * * *`,desc:`Every 6 hours`},{label:`12h`,cron:`0 */12 * * *`,desc:`Every 12 hours`},{label:`1d`,cron:`0 0 * * *`,desc:`Daily at midnight`}];function ya(e){return e===`pending`||e===`active`}function ba(e){let t=e.status||`active`;return e.terminal_reason?`${t} (${e.terminal_reason.replaceAll(`_`,` `)})`:t}function xa(e){return e.retry_count?`${e.retry_count} dispatch ${e.retry_count===1?`retry`:`retries`}`:``}function Sa(e){if(!e)return``;let t=va.find(t=>t.cron===e.trim());return t?t.desc:e.trim().split(/\s+/).length===5?e:`Invalid cron (need 5 fields)`}function Ca(e){let t=e=>String(e).padStart(2,`0`);return`${e.getFullYear()}-${t(e.getMonth()+1)}-${t(e.getDate())}T${t(e.getHours())}:${t(e.getMinutes())}`}function wa(){let e=new Date(Date.now()+5*6e4);return e.setSeconds(0,0),Ca(e)}function Ta(){let e=new Date;return e.setSeconds(0,0),Ca(e)}async function Ea(){let e=oa();try{let t=await ma();sa(t.jobs||t||[],e)}catch(e){console.error(`[jobs] fetch failed:`,e)}}function Da(){let e=a(`recurring`),t=a(``),n=a(wa()),r=a(``),o=a(V.value||``),s=a(``),c=a(``),l=a(!1),u=a(!1);i(()=>{Z.value===`jobs`&&Ea()},[Z.value]),i(()=>{o.value=V.value||``},[V.value]);let f=Sa(t.value),p=e.value===`once`?!!n.value:!!t.value.trim(),m=!!o.value&&p&&!!r.value.trim(),h=async()=>{if(!m)return;s.value=``,c.value=``,l.value=!0;let i=null;try{let a;if(e.value===`once`){let e=new Date(n.value);if(isNaN(e.getTime())){s.value=`Invalid date/time. Please select a valid date.`,l.value=!1;return}a={type:`once`,run_at:e.toISOString()}}else a={type:`recurring`,cron:t.value.trim()};let d={agent_id:o.value,schedule:a,prompt:r.value.trim()};i=`optimistic-job-`+ ++_a,ca({id:i,...d,status:`pending`,next_run_at:null,last_run_at:null});let f=await ha(d);la(i,f),i=null,t.value=``,r.value=``,u.value=!1,n.value=wa(),c.value=e.value===`once`?`Job scheduled (one-time).`:`Recurring job created.`,setTimeout(()=>{c.value=``},4e3)}catch(e){i&&ua(i);let t=e.error?.message||e.message||``;s.value=t||`Failed to create job. Check that all fields are filled and the schedule is valid.`}finally{l.value=!1}},g=async e=>{s.value=``,da(e);try{fa(e,await ga(e))}catch(t){pa(e),t.status===409&&await Ea(),s.value=t.error?.message||t.message||`Failed to cancel job`}};return d`
        <div>
            <div class="jobs-form">
                <select class="jobs-select" value=${o.value}
                        onChange=${e=>{o.value=e.target.value}}>
                    ${H.value.map(e=>d`
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

                ${e.value===`recurring`?d`
                    <div class="cron-presets">
                        ${va.map(e=>d`
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

                    ${u.value&&d`
                        <input class="jobs-input" type="text" placeholder="min hour dom mon dow"
                               value=${t.value}
                               onInput=${e=>{t.value=e.target.value}} />
                    `}

                    ${t.value&&d`
                        <div class="cron-preview">${f}</div>
                    `}
                `:d`
                    <input class="jobs-input" type="datetime-local"
                           value=${n.value}
                           min=${Ta()}
                           onInput=${e=>{n.value=e.target.value}} />
                `}

                <textarea class="jobs-textarea" rows="2" placeholder="Prompt for the agent..."
                          value=${r.value}
                          onInput=${e=>{r.value=e.target.value}}></textarea>

                ${!o.value&&d`
                    <div class="empty-state">
                        No agents available. Create an agent first.
                    </div>
                `}

                <button class="jobs-submit" onClick=${h}
                        disabled=${l.value||!m}>
                    ${l.value?`Scheduling...`:`Schedule`}
                </button>
            </div>

            ${c.value&&d`<div class="jobs-success">${c.value}</div>`}
            ${s.value&&d`<div class="jobs-error">${s.value}</div>`}

            <div class="jobs-divider"></div>

            ${aa.value.length===0?d`<div class="jobs-empty">No scheduled jobs</div>`:aa.value.map(e=>d`
                    <div class="job-item">
                        <div class="job-prompt">${e.prompt||e.task||`(no prompt)`}</div>
                        <div class="job-meta">
                            <span>${Sa(e.schedule?.cron)||(e.schedule?.type===`once`?`Once at `+Mn(e.schedule.run_at):JSON.stringify(e.schedule))}</span>
                            ${e.next_run_at&&d`<span> | next: ${Mn(e.next_run_at)}</span>`}
                            ${e.last_run_at&&d`<span> | last run: ${Mn(e.last_run_at)}</span>`}
                            ${xa(e)&&d`<span> | ${xa(e)}</span>`}
                            ${e.last_error&&d`<span class="job-last-error"> | last error: ${e.last_error}</span>`}
                        </div>
                        <span class="job-status-${e.status||`active`}">${ba(e)}</span>
                        ${ya(e.status||`active`)&&!e.optimistic&&d`
                            <button class="job-cancel" onClick=${()=>g(e.id)}>Cancel</button>
                        `}
                    </div>
                `)}
        </div>
    `}var Oa=(e,t=50)=>f(`/audit?session_id=${e}&limit=${t}`),ka=50;async function Aa(e){if(!E.value){K.value=null;return}try{let t=await Oa(E.value,e);K.value=t.events||t||[]}catch{K.value=[]}}function ja(){let e=a(ka),t=a(!1);i(()=>{Z.value===`audit`&&(e.value=ka,Aa(ka))},[Z.value,E.value]);let n=async()=>{t.value=!0;try{let t=e.value+ka;e.value=t,await Aa(t)}catch(e){console.error(`[AuditTab] loadMore failed:`,e)}finally{t.value=!1}};if(!E.value)return d`<div class="empty-state">No session selected</div>`;if(K.value===null)return d`<div class="loading-state">Loading...</div>`;if(K.value.length===0)return d`<div class="empty-state">No audit events</div>`;let r=K.value.length>=e.value;return d`
        <div>
            ${K.value.map((e,t)=>d`
                <div class="audit-event" key=${e.id||`audit-${e.timestamp||``}-${t}`}>
                    <span class="audit-tool">${e.tool||e.action||`unknown`}</span>
                    <span class="${e.decision===`deny`?`audit-deny`:e.decision===`error`?`audit-error`:`audit-allow`}">
                        ${e.decision===`deny`?`denied`:e.decision===`error`?`error`:`allowed`}
                    </span>
                    ${e.timestamp&&d`<span class="audit-time">${Mn(e.timestamp)}</span>`}
                    ${e.params&&d`
                        <div class="audit-params">${JSON.stringify(e.params).slice(0,120)}</div>
                    `}
                </div>
            `)}
            ${r&&d`
                <button class="audit-load-more"
                        onClick=${n}
                        disabled=${t.value}>
                    ${t.value?`Loading...`:`Load more`}
                </button>
            `}
        </div>
    `}var Ma=50,Na={completed:`✓`,failed:`✗`,cancelled:`⊘`,running:`⋯`},Pa={user:`user`,scheduled:`scheduled`,subagent:`subagent`,dm:`dm`,notification:`notif`,telegram:`telegram`},Fa={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`};function Ia(e){if(e==null)return`--`;if(e<1e3)return e+`ms`;if(e<6e4)return(e/1e3).toFixed(1)+`s`;let t=Math.floor(e/6e4),n=Math.round(e%6e4/1e3);return t+`m`+(n>0?n+`s`:``)}function La(e){return e==null?`--`:e>=1e4?(e/1e3).toFixed(0)+`k`:e>=1e3?(e/1e3).toFixed(1)+`k`:String(e)}function Ra(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function za(){let e=a([]),t=a(!1),n=a(``),r=async()=>{if(!V.value){e.value=[];return}t.value=!0,n.value=``;try{let t=await Ye(V.value,Ma);e.value=t.runs||[]}catch(t){console.error(`[RunsTab] fetch failed:`,t),n.value=t.error?.message||t.message||`Failed to load runs`,e.value=[]}finally{t.value=!1}};return i(()=>{Z.value===`runs`&&r()},[Z.value,V.value,Fe.value]),V.value?t.value&&e.value.length===0?d`<div class="loading-state">Loading runs...</div>`:n.value?d`
            <div>
                <div class="runs-tab-error">${n.value}</div>
                <button class="runs-tab-retry" onClick=${r}>Retry</button>
            </div>
        `:e.value.length===0?d`<div class="runs-tab-empty">No runs yet</div>`:d`
        <div class="runs-tab">
            <div class="runs-tab-header">
                <span class="runs-tab-count">${e.value.length} run${e.value.length===1?``:`s`}</span>
                <button class="runs-tab-refresh" onClick=${r}
                        disabled=${t.value} title="Refresh">
                    ${t.value?`...`:`↻`}
                </button>
            </div>
            <div class="runs-tab-list">
                ${e.value.map(e=>d`
                    <div class="runs-tab-row runs-tab-row--${e.status||`unknown`}"
                         key=${e.run_id}
                         onClick=${()=>e.session_id&&Jt(e.session_id)}
                         title=${`Run `+e.run_id.slice(0,8)+` | Session `+(e.session_id||``).slice(0,8)}>
                        <div class="runs-tab-row-top">
                            <span class="runs-tab-status">${Na[e.status]||`·`}</span>
                            <span class="runs-tab-trigger runs-tab-trigger--${e.trigger||`user`}">
                                ${Pa[e.trigger]||e.trigger||`user`}
                            </span>
                            <span class="runs-tab-session-type">
                                ${Fa[e.session_type]||e.session_type||``}
                            </span>
                            <span class="runs-tab-time">${Ra(e.ts)}</span>
                        </div>
                        <div class="runs-tab-row-bottom">
                            <span class="runs-tab-duration">${Ia(e.duration_ms)}</span>
                            <span class="runs-tab-tools">${e.tool_call_count==null?``:e.tool_call_count+` tools`}</span>
                            <span class="runs-tab-tokens">
                                ${e.usage?La(e.usage.prompt_tokens)+` in / `+La(e.usage.completion_tokens)+` out`+(typeof e.usage.reasoning_tokens==`number`&&e.usage.reasoning_tokens>0?` (+`+La(e.usage.reasoning_tokens)+` reasoning)`:``)+(typeof e.usage.cache_read_input_tokens==`number`&&e.usage.cache_read_input_tokens>0?` (`+La(e.usage.cache_read_input_tokens)+` cached)`:``):``}
                            </span>
                        </div>
                    </div>
                `)}
            </div>
        </div>
    `:d`<div class="runs-tab-empty">No agent selected</div>`}function Ba(e,t=50,n=null){let r=`/agents/${e}/timeline?limit=${t}`;return n&&(r+=`&before=${encodeURIComponent(n)}`),f(r)}var Va=50,Ha={run_started:`▶`,run_completed:`✓`,run_failed:`✗`,run_cancelled:`⊘`,run_ended:`■`,tool_call:`⚙`,message_received:`●`,message_sent:`○`,marker:`⚑`},Ua={run_started:`started`,run_completed:`completed`,run_failed:`failed`,run_cancelled:`cancelled`,run_ended:`ended`,tool_call:`tool`,message_received:`message`,message_sent:`sent`,marker:`marker`},Wa={chat:`chat`,dm:`dm`,subagent:`sub`,job:`job`,notification:`notif`,telegram:`tg`,episodic:`epis`};function Ga(e){if(!e)return``;let t=Date.now()-new Date(e).getTime();if(t<0)return`just now`;let n=Math.floor(t/1e3);if(n<60)return n+`s ago`;let r=Math.floor(n/60);if(r<60)return r+`m ago`;let i=Math.floor(r/60);return i<24?i+`h ago`:Math.floor(i/24)+`d ago`}function Ka(e){return e?new Date(e).toLocaleTimeString([],{hour:`2-digit`,minute:`2-digit`}):``}function qa(e){if(!e)return``;let t=new Date(e),n=new Date,r=new Date;return r.setDate(r.getDate()-1),t.toDateString()===n.toDateString()?`Today`:t.toDateString()===r.toDateString()?`Yesterday`:t.toLocaleDateString([],{weekday:`short`,month:`short`,day:`numeric`})}function Ja(){let e=a([]),t=a(!1),n=a(!1),r=a(``),o=a(!1),s=a(null),c=async(i=!1)=>{if(!V.value){e.value=[];return}i?n.value=!0:t.value=!0,r.value=``;try{let t=i?s.value:null,n=await Ba(V.value,Va,t),r=n.events||[];i?e.value=[...e.value,...r]:e.value=r,o.value=n.pagination?.has_more||!1,s.value=n.pagination?.next_before||null}catch(t){console.error(`[TimelineTab] fetch failed:`,t),r.value=t.error?.message||t.message||`Failed to load timeline`,i||(e.value=[])}finally{t.value=!1,n.value=!1}};if(i(()=>{Z.value===`timeline`&&c(!1)},[Z.value,V.value]),!V.value)return d`<div class="tl-empty">No agent selected</div>`;if(t.value&&e.value.length===0)return d`<div class="loading-state">Loading timeline...</div>`;if(r.value)return d`
            <div>
                <div class="tl-error">${r.value}</div>
                <button class="tl-retry" onClick=${()=>c(!1)}>Retry</button>
            </div>
        `;if(e.value.length===0)return d`<div class="tl-empty">No activity yet</div>`;let l=new Set;{let t=``;for(let n of e.value){let e=qa(n.timestamp);e!==t&&(l.add(n),t=e)}}return d`
        <div class="tl-tab">
            <div class="tl-header">
                <span class="tl-count">${e.value.length} event${e.value.length===1?``:`s`}</span>
                <button class="tl-refresh" onClick=${()=>c(!1)}
                        disabled=${t.value} title="Refresh">
                    ${t.value?`...`:`↻`}
                </button>
            </div>
            <div class="tl-list">
                ${e.value.map((e,t)=>{let n=qa(e.timestamp),r=l.has(e),i=e.event_type===`tool_call`,a=e.event_type===`run_started`||e.event_type===`run_completed`||e.event_type===`run_failed`||e.event_type===`run_cancelled`||e.event_type===`run_ended`,o=e.metadata?.tool_name,s=e.event_type+`-`+e.timestamp+`-`+(e.run_id||``)+`-`+t+(o?`-`+o:``);return d`
                        ${r&&d`
                            <div class="tl-date-group" key=${`g-`+n}>${n}</div>
                        `}
                        <div class="tl-event tl-event--${e.event_type}${i?` tl-event--indent`:``}${a?` tl-event--run`:``}"
                             key=${s}
                             onClick=${()=>Jt(e.session_id)}
                             title=${`Session `+(e.session_id||``).slice(0,8)+(e.run_id?` | Run `+e.run_id.slice(0,8):``)}>
                            <span class="tl-time">${Ka(e.timestamp)}</span>
                            <span class="tl-icon tl-icon--${e.event_type}">${Ha[e.event_type]||`·`}</span>
                            <span class="tl-session-badge tl-session-badge--${e.session_type||`chat`}">
                                ${Wa[e.session_type]||e.session_type||`chat`}
                            </span>
                            <span class="tl-event-label">${Ua[e.event_type]||e.event_type}</span>
                            <span class="tl-ago">${Ga(e.timestamp)}</span>
                        </div>
                        ${e.summary&&d`
                            <div class="tl-summary${i?` tl-summary--indent`:``}"
                                 onClick=${()=>Jt(e.session_id)}>
                                ${e.summary}
                            </div>
                        `}
                    `})}
            </div>
            ${o.value&&d`
                <button class="tl-load-more"
                        onClick=${()=>c(!0)}
                        disabled=${n.value}>
                    ${n.value?`Loading...`:`Load more`}
                </button>
            `}
        </div>
    `}function Ya({tab:e}){return e===`agents`?d`<${Xi} />`:e===`workspace`?d`<${ia} />`:e===`runs`?d`<${za} />`:e===`jobs`?d`<${Da} />`:e===`audit`?d`<${ja} />`:e===`timeline`?d`<${Ja} />`:null}function Xa(){X.value=null}function Za(){return X.value?d`
        <div id="panel" class="open">
            <div class="panel-header">
                <span class="panel-header-title">${Z.value.charAt(0).toUpperCase()+Z.value.slice(1)}</span>
                <button class="panel-close-btn" title="Close panel" aria-label="Close panel"
                        onClick=${Xa}>\u00D7</button>
            </div>
            <div class="panel-body">
                <${Ya} tab=${Z.value} />
            </div>
        </div>
    `:null}var Qa=()=>f(`/auth/keys`),$a=(e,t)=>_(`/auth/keys`,{provider:e,key:t}),eo=e=>ne(`/auth/keys/${e}`),to=[`openai`,`anthropic`,`openrouter`,`gemini`];function no({title:e,defaultOpen:t=!1,children:n}){let r=a(t);return d`
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
    `}function ro({label:e,value:t,desc:n}){return d`
        <div class="settings-info-row">
            <div class="settings-info-row-header">
                <span class="settings-info-row-label">${e}</span>
                <span class="settings-info-row-value">${t}</span>
            </div>
            ${n&&d`<span class="settings-hint">${n}</span>`}
        </div>
    `}function $({label:e,desc:t,children:n}){return d`
        <div class="settings-info-row">
            <div class="settings-info-row-header" style="flex-wrap:wrap;gap:6px;">
                <span class="settings-info-row-label">${e}</span>
                ${n}
            </div>
            ${t&&d`<span class="settings-hint">${t}</span>`}
        </div>
    `}function io(){let e=a([]),t=a(null),n=a(``),r=a(!1),o=a(``),s=async()=>{try{let t=await Qa();e.value=t.keys||[]}catch(e){console.error(`[auth] list keys failed:`,e)}};i(()=>{s()},[]);let c=async e=>{if(n.value.trim()){r.value=!0,o.value=``;try{await $a(e,n.value.trim()),n.value=``,t.value=null,await s()}catch(e){o.value=e.error?.message||e.message||`Failed to save key`}finally{r.value=!1}}},l=async e=>{try{await eo(e),await s()}catch(e){o.value=e.error?.message||e.message||`Failed to remove key`}};return d`
        <div class="settings-row">
            <label class="settings-label">API Keys</label>
            ${to.map(i=>{let a=e.value.find(e=>e.provider===i),o=a?.configured,s=a?.source||`none`,u=a?.key||``;return t.value===i?d`
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
                    `:d`
                    <div class="api-key-row" key=${i}>
                        <span class="api-key-provider">${i}</span>
                        <span class="api-key-value ${o?`set`:`unset`}">
                            ${o?u:`not configured`}
                        </span>
                        ${o&&s===`secrets`&&d`
                            <span class="api-key-source">stored</span>
                        `}
                        <div class="api-key-actions">
                            <button class="api-key-btn" onClick=${()=>{t.value=i,n.value=``}}>
                                ${o?`Change`:`Set`}
                            </button>
                            ${o&&s===`secrets`&&d`
                                <button class="api-key-btn remove" onClick=${()=>l(i)}>Remove</button>
                            `}
                        </div>
                    </div>
                `})}
            ${o.value&&d`<div class="inline-error">${o.value}</div>`}
        </div>
    `}function ao({open:e,onClose:t}){let n=a(!1),r=a(!1),o=a(``),s=a(``),c=a(``),l=a(``),u=a(``),f=a(``),p=a(``),m=a(``),h=a(``),g=a(!0),_=a(``),v=a(``),y=a(``),b=a(``),x=a(``),S=a(``),C=a(``),w=a(``),T=a(!0),E=a(!1),D=a(``),ee=a(``),te=a(!0),ne=a(!1),re=a(``),ie=a(!1),ae=a(!1),O=a(!1),k=a(``);if(i(()=>{if(e){let e=W.value,t=e.context||{},i=e.session||{},a=e.tools||{},d=e.llm||{},O=d.anthropic||{},A=d.openai||{},j=d.gemini||{};o.value=t.strategy||`truncate`,s.value=t.max_input_tokens==null?``:String(t.max_input_tokens),c.value=t.compact_trigger_pct==null?``:String(t.compact_trigger_pct),l.value=t.compact_retain_pct==null?``:String(t.compact_retain_pct),u.value=t.summary_model||``,f.value=t.summary_provider||``,p.value=i.max_messages==null?``:String(i.max_messages),m.value=i.max_context_tokens==null?``:String(i.max_context_tokens),h.value=i.idle_timeout_secs==null?``:String(i.idle_timeout_secs),g.value=i.auto_archive==null||i.auto_archive,_.value=i.archive_ttl_secs==null?``:String(i.archive_ttl_secs),v.value=a.shell_policy||`sandboxed`,y.value=a.sandbox_root||`.`,b.value=a.timeout_secs==null?``:String(a.timeout_secs),x.value=a.max_output_bytes==null?``:String(a.max_output_bytes),S.value=e.model||``,C.value=e.provider||``,w.value=O.thinking_budget_tokens==null?``:String(O.thinking_budget_tokens),T.value=O.prompt_cache_enabled==null||!!O.prompt_cache_enabled,E.value=!1,D.value=A.reasoning_effort||``,ee.value=j.thinking_budget==null?``:String(j.thinking_budget),te.value=j.cache_enabled==null||!!j.cache_enabled,ne.value=!1,re.value=j.cache_ttl_seconds==null?``:String(j.cache_ttl_seconds);let M=H.value.find(e=>e.id===V.value);ie.value=!!(M&&M.debug_mode),ae.value=!1,n.value=!1,r.value=!1,k.value=``}},[e]),!e)return null;let A=W.value,j=A.context||{},M=A.session||{},N=A.logging||{},oe=A.tools||{},P=A.llm||{},se=P.anthropic||{},ce=P.openai||{},F=P.gemini||{},le=async()=>{O.value=!0,k.value=``,n.value=!1;let e={},i={};o.value&&o.value!==(j.strategy||``)&&(i.strategy=o.value);let a=parseInt(s.value,10);!isNaN(a)&&a!==j.max_input_tokens&&(i.max_input_tokens=a);let d=parseFloat(c.value);!isNaN(d)&&d!==j.compact_trigger_pct&&(i.compact_trigger_pct=d);let N=parseFloat(l.value);!isNaN(N)&&N!==j.compact_retain_pct&&(i.compact_retain_pct=N),u.value!==(j.summary_model||``)&&(i.summary_model=u.value),f.value!==(j.summary_provider||``)&&(i.summary_provider=f.value),Object.keys(i).length>0&&(e.context=i);let P={},le=parseInt(p.value,10);!isNaN(le)&&le!==M.max_messages&&(P.max_messages=le);let I=parseInt(m.value,10);!isNaN(I)&&I!==M.max_context_tokens&&(P.max_context_tokens=I);let L=parseInt(h.value,10);!isNaN(L)&&L!==M.idle_timeout_secs&&(P.idle_timeout_secs=L),g.value!==M.auto_archive&&(P.auto_archive=g.value);let ue=parseInt(_.value,10);!isNaN(ue)&&ue!==M.archive_ttl_secs&&(P.archive_ttl_secs=ue),Object.keys(P).length>0&&(e.session=P);let R={};v.value&&v.value!==(oe.shell_policy||``)&&(R.shell_policy=v.value),y.value!==(oe.sandbox_root||``)&&(R.sandbox_root=y.value);let de=parseInt(b.value,10);!isNaN(de)&&de!==oe.timeout_secs&&(R.timeout_secs=de);let fe=parseInt(x.value,10);!isNaN(fe)&&fe!==oe.max_output_bytes&&(R.max_output_bytes=fe),Object.keys(R).length>0&&(e.tools=R);let pe={},me={},he=parseInt(w.value,10);w.value!==``&&!isNaN(he)&&he!==se.thinking_budget_tokens&&(me.thinking_budget_tokens=he),E.value&&T.value!==!!se.prompt_cache_enabled&&(me.prompt_cache_enabled=T.value),Object.keys(me).length>0&&(pe.anthropic=me);let ge={},_e=ce.reasoning_effort||``;D.value!==_e&&(ge.reasoning_effort=D.value),Object.keys(ge).length>0&&(pe.openai=ge);let ve={},ye=parseInt(ee.value,10);ee.value!==``&&!isNaN(ye)&&ye!==F.thinking_budget&&(ve.thinking_budget=ye),ne.value&&te.value!==!!F.cache_enabled&&(ve.cache_enabled=te.value);let be=parseInt(re.value,10);re.value!==``&&!isNaN(be)&&be!==F.cache_ttl_seconds&&(ve.cache_ttl_seconds=be),Object.keys(ve).length>0&&(pe.gemini=ve),Object.keys(pe).length>0&&(e.llm=pe),S.value&&S.value!==(A.model||``)&&(e.model=S.value),C.value&&C.value!==(A.provider||``)&&(e.provider=C.value);let xe=!1;if(Object.keys(e).length>0)try{let t=await et(e);t&&t.restart_required&&(xe=!0),await tt()}catch(e){let t=Array.isArray(e.errors)?e.errors.join(`; `):null;k.value=t||e.message||`Failed to save server settings`}if(ae.value&&V.value){let e=H.value.find(e=>e.id===V.value),t=Vi(e&&e.debug_mode,ie.value);if(Object.keys(t).length>0)try{await xi(V.value,t);let e=await yi();e&&Array.isArray(e.agents)&&Me(e.agents)}catch(e){k.value=e.error?.message||e.message||`Failed to save debug mode`}}k.value||(n.value=!0,r.value=xe),O.value=!1,!k.value&&!xe&&setTimeout(()=>t(),600)},I=e=>{e.target===e.currentTarget&&t()},L=oe.enabled||A.enabled_tools||[];return d`
        <div class="settings-overlay open" onClick=${I}>
            <div class="settings-modal">
                <h2>Settings</h2>

                <!-- Security: API Keys -->
                <${io} />

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
                <${no} key="debug" title="Debug" defaultOpen=${!1}>
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
                                   checked=${ie.value}
                                   disabled=${!V.value}
                                   onChange=${e=>{ie.value=e.target.checked,ae.value=!0}} />
                            <span>${ie.value?`enabled`:`disabled`}</span>
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
                <${no} key="defaults" title="Default LLM (model / provider)" defaultOpen=${!0}>
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
                               placeholder=${A.model||`model id`}
                               value=${S.value}
                               onInput=${e=>{S.value=e.target.value}} />
                        <span class="settings-effective">
                            <${ji} value=${S.value.trim()} defaultValue=${A.model} />
                        </span>
                    <//>
                    <${$} label="Default LLM provider"
                        desc="Provider whose [llm.providers.NAME] entry the resolved model is sent to. Must be configured under [llm.providers] in alms.toml with a resolvable API key.">
                        <select class="settings-select settings-input-sm"
                                value=${C.value}
                                onChange=${e=>{C.value=e.target.value}}>
                            ${(A.llm_providers&&A.llm_providers.length>0?A.llm_providers:to).map(e=>{let t=ki(e);return d`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                    <datalist id="model-suggestions">
                        ${Ti.map(e=>d`<option value=${e} key=${e}></option>`)}
                    </datalist>
                <//>

                <!-- Context (server-level, editable) -->
                <${no} key="ctx" title="Context" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        truncate fits the most recent history into the token budget.
                        compact summarises older messages once the session crosses the trigger threshold.
                        Changes apply to the next run.
                    </span>
                    <${$} label="Strategy"
                        desc="truncate = drop oldest messages to fit the budget. compact = summarise old + keep recent verbatim once history crosses the trigger threshold.">
                        <select class="settings-select settings-input-sm"
                                value=${o.value}
                                onChange=${e=>{o.value=e.target.value}}>
                            <option value="truncate">truncate — drop oldest messages to fit budget</option>
                            <option value="compact">compact — summarise old + keep recent verbatim</option>
                        </select>
                    <//>
                    <${$} label="Max input tokens"
                        desc="Token budget per LLM request (should match your model's context window).">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${s.value}
                               onInput=${e=>{s.value=e.target.value}} />
                    <//>
                    ${o.value===`compact`?d`
                    <${$} label="Compact trigger %"
                        desc="Compact strategy: trigger compaction when assembled history exceeds this fraction of the effective history budget (max_input_tokens minus system / input / episodic / reserve overhead). Range: 0.50–0.95.">
                        <input class="settings-input settings-input-sm" type="number"
                               min="0.50" max="0.95" step="0.05"
                               value=${c.value}
                               onInput=${e=>{c.value=e.target.value}} />
                    <//>
                    <${$} label="Compact retain %"
                        desc="Compact strategy: retain at most this fraction of the effective history budget (max_input_tokens minus system / input / episodic / reserve overhead) worth of recent verbatim messages after compaction. Range: 0.20–0.60.">
                        <input class="settings-input settings-input-sm" type="number"
                               min="0.20" max="0.60" step="0.05"
                               value=${l.value}
                               onInput=${e=>{l.value=e.target.value}} />
                    <//>
                    `:null}
                <//>

                <!-- Summary (server-level, editable) — controls BOTH the
                     in-loop compact-strategy compaction AND the post-run
                     episodic memory generation. Lifted out of the Context
                     section to make the dual-path scope obvious. -->
                <${no} key="summary" title="Summary (compact strategy + episodic memory)" defaultOpen=${!1}>
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
                               value=${u.value}
                               onInput=${e=>{u.value=e.target.value}} />
                        <span class="settings-effective">
                            <${ji} value=${u.value.trim()} defaultValue=${A.model} />
                        </span>
                    <//>
                    <${$} label="Summary provider"
                        desc="Dedicated provider for the summary task. Must be configured under [llm.providers.<name>] with a resolvable API key. Set together with Summary model.">
                        <select class="settings-select settings-input-sm"
                                value=${f.value}
                                onChange=${e=>{f.value=e.target.value}}>
                            <option value="">Unset (no dedicated summary task)</option>
                            ${(A.llm_providers&&A.llm_providers.length>0?A.llm_providers:to).map(e=>{let t=ki(e);return d`<option value=${e} key=${e}>${t===`Custom`?e:t}</option>`})}
                        </select>
                    <//>
                <//>

                <!-- Session (server-level, editable) -->
                <${no} key="sess" title="Session" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Controls session storage and retention. Changes apply to the next run.
                    </span>
                    <${$} label="Max messages"
                        desc="Maximum messages stored per session.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${p.value}
                               onInput=${e=>{p.value=e.target.value}} />
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
                <${no} key="tools" title="Tools" defaultOpen=${!1}>
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
                    <${ro} label="Enabled tools" value=${`${L.length} tools`}
                        desc=${L.join(`, `)} />
                <//>

                <!-- LLM Providers (server-level, editable) — #809 / #804 Slice A -->
                <${no} key="llm" title="LLM Providers" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        Server-level reasoning &amp; caching defaults. Mutations propagate to the next HTTP-triggered run without restart; Telegram-triggered runs use a boot-time snapshot until the daemon is restarted.
                    </span>

                    <h4 class="settings-llm-subhead">Anthropic</h4>
                    <${$} label="Thinking budget tokens"
                        desc="0 = extended thinking off. Leave blank to keep the current server value. The wire surface has no clear sentinel — once PATCHed, revert by editing settings.json + restart.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${se.thinking_budget_tokens==null?`unset`:String(se.thinking_budget_tokens)}
                               value=${w.value}
                               onInput=${e=>{w.value=e.target.value}} />
                    <//>
                    <${$} label="Prompt cache enabled"
                        desc="Anthropic prefix caching (5-minute TTL). Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${T.value}
                                   onChange=${e=>{T.value=e.target.checked,E.value=!0}} />
                            <span>${T.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>

                    <h4 class="settings-llm-subhead">OpenAI / OpenRouter</h4>
                    <${$} label="Reasoning effort"
                        desc="Applies to o-series, GPT-5, and reasoning-capable Grok models. Auto-stripped on non-reasoning models. Choose Unset to clear an existing override.">
                        <select class="settings-select settings-input-sm"
                                value=${D.value}
                                onChange=${e=>{D.value=e.target.value}}>
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
                               placeholder=${F.thinking_budget==null?`unset`:String(F.thinking_budget)}
                               value=${ee.value}
                               onInput=${e=>{ee.value=e.target.value}} />
                    <//>
                    <${$} label="Cache enabled"
                        desc="Gemini context caching via cachedContents. Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${te.value}
                                   onChange=${e=>{te.value=e.target.checked,ne.value=!0}} />
                            <span>${te.value?`enabled`:`disabled`}</span>
                        </label>
                    <//>
                    <${$} label="Cache TTL (seconds)"
                        desc="Lifetime of a Gemini cache entry. Must be > 0.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="60"
                               placeholder=${F.cache_ttl_seconds==null?`300`:String(F.cache_ttl_seconds)}
                               value=${re.value}
                               onInput=${e=>{re.value=e.target.value}} />
                    <//>
                <//>

                <!-- Logging (server-level, read-only) -->
                <${no} key="log" title="Logging" defaultOpen=${!1}>
                    <span class="settings-hint settings-section-desc">
                        File-based logging settings. Requires restart to change.
                    </span>
                    <${ro} label="File logging" value=${N.file_enabled==null?`--`:N.file_enabled?`enabled`:`disabled`}
                        desc="Whether persistent file logging is active." />
                    <${ro} label="File level" value=${N.file_level||`--`}
                        desc="Log level for file output (trace, debug, info, warn, error)." />
                    <${ro} label="Rotation" value=${N.rotation||`--`}
                        desc="Log rotation policy: daily, hourly, or never." />
                    <${ro} label="Log directory" value=${N.log_dir||`default (data/logs/)`}
                        desc="Directory where log files are written." />
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

                ${k.value&&d`
                    <div class="settings-error">
                        Failed to save server settings: ${k.value}
                    </div>
                `}

                ${r.value&&d`
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
                    <button class="settings-save" onClick=${le}
                            disabled=${O.value}>
                        ${O.value?`Saving...`:n.value?`Saved!`:`Apply`}
                    </button>
                </div>
            </div>
        </div>
    `}function oo(){let e=a(``),t=a(``),n=a(!1);return d`
        <div id="onboarding">
            <form class="onboard-card" onSubmit=${async r=>{r.preventDefault();let i=e.value.trim();if(i){if(!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(i)){t.value=`Invalid name: lowercase letters, digits, hyphens only (1-64 chars, no trailing hyphen)`;return}n.value=!0,t.value=``;try{let e=await bi({name:i,is_default:!0});Me((await yi()).agents||[]);let t=e.id||(H.value.find(e=>e.name===i)||{}).id;t?await Et(t):console.warn(`[onboarding] POST /agents returned no id for agent:`,i,e)}catch(e){t.value=e.error?.message||e.message||`Failed to create agent`}finally{n.value=!1}}}}>
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
    `}function so(e){if(!e)return``;if(e.status===`done`)return`Done`;if(e.status===`fail`)return`Failed`;if(e.status===`cancelled`)return`Cancelled`;let t=e.activity;if(!t||!t.kind)return`Starting…`;switch(t.kind){case`reasoning`:return`Reasoning…`;case`writing`:return`Writing…`;case`tool_start`:return t.tool?`Using ${t.tool}`:`Using tool`;case`tool_end`:return`Running…`;default:return`Running…`}}function co(){let e=Object.entries(je.value);return e.length===0?null:d`
        <div class="sa-bar" aria-label="Subagent status bar">
            ${e.map(([e,t])=>{let n=t.status===`running`,r=t.status===`done`?`✓`:`✗`,i=t.displayName||e,a=so(t),o=()=>{t.sessionId&&fe(t.sessionId)},s=e=>{Bi(e)&&o()},c=e=>{zi(e)&&(e.preventDefault(),o())},l=t.task?`${i}: ${t.task} — open subagent session`:`${i} — open subagent session`,u=le(t.sessionId),f=e=>t=>{if(t.stopPropagation(),t.key===`Escape`){t.preventDefault(),I();return}(t.key===`Enter`||t.key===` `)&&(t.preventDefault(),e(t))},p=e=>{e.stopPropagation(),R(t.sessionId)},m=e=>{e.stopPropagation(),pe(t.sessionId)},h=e=>{e.stopPropagation(),I()};return d`
                    <div class="sa-chip ${n?`running`:t.status}"
                         role="button"
                         tabindex="0"
                         title=${l}
                         onClick=${s}
                         onKeyDown=${c}>
                        ${n?d`<span class="tc-spinner"></span>`:d`<span>${r}</span>`}
                        <span class="sa-chip-name">${i}</span>
                        ${a&&d`<span class="sa-chip-status">${a}</span>`}
                        ${xe(t)&&(u?d`
                                <span class="sa-cancel-confirm-group" role="group"
                                      aria-label="Confirm cancel subagent">
                                    <span class="sa-cancel-confirm-label">Cancel?</span>
                                    <button class="sa-confirm-btn sa-confirm-yes"
                                            title="Yes, cancel this subagent"
                                            aria-label="Yes, cancel this subagent"
                                            onClick=${m}
                                            onKeyDown=${f(m)}>Yes</button>
                                    <button class="sa-confirm-btn sa-confirm-no"
                                            title="No, keep it running"
                                            aria-label="No, keep it running"
                                            onClick=${h}
                                            onKeyDown=${f(h)}>No</button>
                                </span>
                            `:d`
                                <button class="sa-chip-cancel"
                                        title="Cancel this subagent"
                                        aria-label="Cancel this subagent"
                                        onClick=${p}
                                        onKeyDown=${f(p)}>✕</button>
                            `)}
                    </div>
                `})}
        </div>
    `}function lo(){let e=B.value,{phase:t,detail:n}=Te.value,r=ve(t,n),o=a(!1),s=a(!1),u=c(null),f=l(()=>{o.value=!o.value},[]),p=l(()=>{o.value=!1,s.value=!0},[]);return i(()=>{if(!o.value)return;let e=e=>{u.current&&!u.current.contains(e.target)&&(o.value=!1)};return document.addEventListener(`click`,e,!0),()=>document.removeEventListener(`click`,e,!0)},[o.value]),e?d`
        <div class="agent-header-bar">
            <div class="agent-header-bar-left">
                <span class="agent-header-bar-name">${e.name}</span>
                ${r&&d`
                    <span class="agent-status-label">${r}</span>
                `}
            </div>
            <div class="agent-header-bar-right">
                <button class="hbtn agent-bar-btn ${X.value===`workspace`?`active`:``}"
                        title="Workspace files"
                        aria-label="Open workspace panel"
                        onClick=${()=>Dt(`workspace`)}>
                    <${Nt} />
                    <span class="agent-bar-btn-label">Workspace</span>
                </button>
                <button class="hbtn agent-bar-btn ${X.value===`timeline`?`active`:``}"
                        title="Agent timeline"
                        aria-label="Open timeline panel"
                        onClick=${()=>Dt(`timeline`)}>
                    <${Mt} />
                    <span class="agent-bar-btn-label">Timeline</span>
                </button>
                <button class="hbtn agent-bar-btn ${X.value===`runs`?`active`:``}"
                        title="Agent runs"
                        aria-label="Open runs panel"
                        onClick=${()=>Dt(`runs`)}>
                    <${Lt} />
                    <span class="agent-bar-btn-label">Runs</span>
                </button>
                <div class="agent-menu-anchor" ref=${u}>
                    <button class="hbtn agent-bar-btn"
                            title="Agent menu"
                            aria-label="Open agent menu"
                            aria-expanded=${o.value}
                            onClick=${f}>
                        <span class="agent-menu-dots" aria-hidden="true">\u22EF</span>
                    </button>
                    ${o.value&&d`
                        <div class="agent-menu-dropdown">
                            <button class="agent-menu-item" onClick=${p}>
                                Settings
                            </button>
                        </div>
                    `}
                </div>
            </div>

            ${s.value&&d`
                <${Ji}
                    agent=${e}
                    onClose=${()=>{s.value=!1}} />
            `}
        </div>
    `:null}var uo=o(!1),fo=new Set;function po(e,t,n){return e.fromAgent?e.fromAgent===t[0]?`left`:`right`:e.type===`agent`||e.role===`assistant`?n?n===t[0]?`left`:`right`:`left`:e.type===`user`||e.role===`user`?n?n===t[0]?`right`:`left`:`right`:`center`}function mo({msg:e,participants:t,perspectiveAgent:n}){let r=po(e,t,n),a=e.fromAgent||(r===`left`?t[0]:t[1])||`?`,o=u(e.text||``),s=e.type===`agent`||e.role===`assistant`,l=c(null);return i(()=>{s&&Yn(l.current)},[o,s]),d`
        <div class="dm-msg dm-msg-${r}">
            <div class="dm-msg-name-row dm-msg-name-row-${r}">
                <div class="dm-msg-name">${a}</div>
                <${Xn} ts=${e.ts} />
            </div>
            <div class="dm-msg-bubble markdown-body" ref=${l}
                 dangerouslySetInnerHTML=${{__html:o}} />
        </div>
    `}function ho({text:e}){return d`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">${e}</span>
        </div>
    `}function go(e,t){if(!e)return!1;let n=e.trim();if(!n)return!1;for(let e of t||[]){if(e.tool!==`send_message`)continue;let t=e.params&&typeof e.params.message==`string`?e.params.message.trim():``;if(t&&t===n)return!0}return!1}function _o({runId:e,agentName:n,thinkingText:r,tools:i,status:a,isLive:o}){let[s,c]=t(!1),l=o&&Ce.value.get(e)||``,u=r||l,f=go(u,i)?``:u,p=(i||[]).filter(e=>!(e.tool===`send_message`&&e.status===`done`)),m=p.length,h=(i||[]).length>0;return!o&&!h&&(!f||!f.trim())?null:d`
        <div class=${`dm-reasoning-block`+(a===`failed`?` dm-reasoning-block--failed`:``)+(o?` dm-reasoning-block--live`:``)}>
            <div class="dm-reasoning-header" onClick=${()=>c(!s)}>
                <span class="dm-reasoning-toggle">${s?`▼`:`▶`}</span>
                <span class="dm-reasoning-summary">${n?`${n} reasoning -- ${m} tool call${m===1?``:`s`}`:`Agent reasoning -- ${m} tool call${m===1?``:`s`}`}</span>
                ${o&&d`<span class="dm-reasoning-spinner" />`}
            </div>
            ${s&&d`
                <div class="dm-reasoning-body">
                    ${f&&f.trim()&&d`
                        <pre class="dm-reasoning-thinking">${f}</pre>
                    `}
                    ${p.map(e=>d`
                        <${Hr} key=${e.id} ...${e} />
                    `)}
                </div>
            `}
        </div>
    `}async function vo(){let e=E.value;if(!(!e||uo.value)){uo.value=!0;try{await ce(e)}catch(e){console.error(`[cancel-dm] failed:`,e)}finally{uo.value=!1}}}function yo(){let e=c(null),t=se.value;i(()=>{let t=0,n=r(()=>{ae.value,cancelAnimationFrame(t),t=requestAnimationFrame(()=>{Pn(e.current)})});return()=>{cancelAnimationFrame(t),n()}},[]);let n=ae.value,a=B.value?B.value.name:null,o=t.length>=2?`${t[0]} <-> ${t[1]}`:`DM conversation`,s=!!U.value,l=!!ye.value,u=s||l,f=uo.value;return d`
        <div class="dm-view-header">
            <span class="dm-view-header-icon" aria-hidden="true">\u2194</span>
            <span class="dm-view-header-label">${o}</span>
            <span class="dm-view-header-badge">read-only</span>
        </div>
        <div class="dm-thread" ref=${e}>
            ${n.length===0&&d`
                <div class="empty-state">No messages in this conversation yet.</div>
            `}
            ${n.map(e=>{if(e.type===`dm_ended`){let t=`Conversation ended -- ${e.reason||`ended`}`;return d`<${ho} key=${e.id} text=${t} />`}if(e.type===`system`)return d`<${ho} key=${e.id} text=${e.text} />`;if(e.type===`notification`){let t=e.metadata||{};if(t.type===`dm_ended_notification`){let n=`DM with ${t.peer||`unknown`} ended -- ${_e[t.reason]||t.reason||`ended`}`;return d`<${ho} key=${e.id} text=${n} />`}return d`<${ho} key=${e.id} text=${e.text} />`}if(e.type===`error`)return d`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-error">${e.text}</div></div>`;if(e.type===`tokens`)return null;if(e.type===`thinking`){let t=`Thinking…`;if(e.pending)t=`Sending…`;else if(e.queuedBehind>0)t=`Queued \u2014 position ${e.queuedBehind}\u2026`;else if(e.source){let n=e.source.startsWith(`peer:`)?e.source.slice(5):e.source;n&&(t=`${n} is thinking\u2026`)}return d`<div key=${e.id} class="dm-msg dm-msg-center"><div class="dm-msg-thinking">${t}</div></div>`}if(e.type===`warning`)return d`<${ho} key=${e.id} text=${e.text||`Warning`} />`;if(e.type===`run_boundary`){if(!e.status||e.status===`completed`)return null;let t=e.status===`failed`?`run failed`:e.status===`cancelled`?`run cancelled`:`run ${e.status}`;return d`<${ho} key=${e.id} text=${t} />`}if(e.type===`subagent_completed`){let t=`Subagent '${e.name||`subagent`}' ${e.status===`fail`?`failed`:`completed`}`;return d`<${ho} key=${e.id} text=${t} />`}if(e.type===`job_completed`)return d`<${ho} key=${e.id} text=${`Job '${e.jobName||`job`}' ${e.status||`completed`}`} />`;if(e.type===`context_debug`)return d`<${Jr} key=${e.id} ...${e} />`;if(e.type===`dm_reasoning`)return d`<${_o} key=${e.id} ...${e} />`;if(e.type===`tool`){if(e.tool===`send_message`&&e.status===`done`&&!e.error)return null;fo.has(e.id)||(fo.add(e.id),console.warn(`[DmConversationView] ungrouped DM tool rendered as a standalone sibling row — this fallback is meant to be dead post-#1076/#1154. Tool:`,e.tool,`id:`,e.id,`runId:`,e.runId));let n=po({type:`agent`,role:`assistant`},t,a),r=n===`left`?t[0]:t[1];return d`
                        <div key=${e.id} class="dm-msg dm-msg-${n} dm-msg-tool-row">
                            <div class="dm-msg-name">${r||`?`}</div>
                            <${Hr} ...${e} />
                        </div>
                    `}if(e.type===`image`){let n=po(e,t,a),r=e.fromAgent||(n===`left`?t[0]:t[1])||`?`;return d`
                        <div key=${e.id} class="dm-msg dm-msg-${n}">
                            <div class="dm-msg-name-row dm-msg-name-row-${n}">
                                <div class="dm-msg-name">${r}</div>
                                <${Xn} ts=${e.ts} />
                            </div>
                            <div class="dm-msg-bubble">
                                ${e.url?d`<img src=${e.url} alt=${e.alt||``} class="dm-msg-image" />`:`[Image${e.alt?`: `+e.alt:``}]`}
                            </div>
                        </div>
                    `}return e.type===`user`||e.type===`agent`?d`<${mo} key=${e.id} msg=${e} participants=${t} perspectiveAgent=${a} />`:null})}
        </div>
        <div class="dm-view-footer">
            ${u?d`
                    <button class="dm-cancel-btn"
                            disabled=${f}
                            title="Stop this DM conversation"
                            aria-label="Stop conversation"
                            onClick=${vo}>
                        <span class="dm-cancel-btn-icon" aria-hidden="true">\u25A0</span>
                        ${f?`Stopping…`:`Stop conversation`}
                    </button>
                `:d`
                    <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
                `}
        </div>
    `}function bo(){return ke.value?d`
        <button
            type="button"
            class="stream-dead-banner"
            role="alert"
            aria-live="polite"
            onClick=${ge}
            title="Click to reconnect live updates"
        >
            <span class="stream-dead-banner-icon" aria-hidden="true">⚠</span>
            <span class="stream-dead-banner-text">
                Live updates disconnected — click to reconnect or reload.
            </span>
        </button>
    `:null}r(()=>{let e=B.value;document.title=e?`ALMS - ${e.name}`:`ALMS`});var xo=o(`connecting...`);function So(e){let t=[],n=0;for(;n<e.length;)if(e[n].type===`tool`){let r=[];for(;n<e.length&&e[n].type===`tool`;)r.push(e[n]),n++;r.length>1?t.push({_isToolGroup:!0,key:`tg-`+r[0].id,tools:r}):t.push(r[0])}else t.push(e[n]),n++;return t}function Co(){let e=c(null);i(()=>{let t=0,n=r(()=>{ae.value,cancelAnimationFrame(t),t=requestAnimationFrame(()=>{Pn(e.current)})});return()=>{cancelAnimationFrame(t),n()}},[]);let t=So(ae.value),n=P.value,a=v.value,o=p.value,s=D.value,l=b.value,u=o?s?.agent_name?s.agent_name+` notifications`:`Notification session`:s?.session_type===`job`?l?l+` job session`:`Job session`:s?.session_type===`subagent`?`Subagent session`:`Internal session`,f=o?`⚡`:s?.session_type===`job`?`⏰`:`⚙`,m=s?.session_type?`internal-session-`+s.session_type:``;return d`
        <div id="chat">
            <${lo} />
            ${(Ve.value||We.value)&&d`
                <div id="messages" role="log" aria-live="polite">
                    ${Ve.value?d`<div class="loading-state">Loading agent...</div>`:d`<div class="loading-state">Loading session...</div>`}
                </div>
            `}
            ${!Ve.value&&!We.value&&n&&d`
                <${yo} />
            `}
            ${!Ve.value&&!We.value&&!n&&d`
            ${a&&d`
                <div class="internal-session-header ${m}">
                    <span class="internal-session-header-icon" aria-hidden="true">${f}</span>
                    <span class="internal-session-header-label">${u}</span>
                    <span class="internal-session-header-badge">read-only</span>
                </div>
            `}
            ${F.value&&d`
                <div class="sa-breadcrumb">
                    <button class="sa-breadcrumb-btn" onClick=${()=>Ae()}>
                        \u2190 Back to parent session
                    </button>
                    ${U.value&&(le(E.value)?d`
                            <span class="sa-cancel-confirm-group sa-breadcrumb-cancel" role="group"
                                  aria-label="Confirm cancel subagent"
                                  onKeyDown=${e=>{e.key===`Escape`&&(e.preventDefault(),I())}}>
                                <span class="sa-cancel-confirm-label">Cancel this subagent?</span>
                                <button class="sa-confirm-btn sa-confirm-yes"
                                        title="Yes, cancel this subagent"
                                        onClick=${()=>pe(E.value)}>Yes</button>
                                <button class="sa-confirm-btn sa-confirm-no"
                                        title="No, keep it running"
                                        onClick=${()=>I()}>No</button>
                            </span>
                        `:d`
                            <button class="sa-breadcrumb-cancel-btn sa-breadcrumb-cancel"
                                    title="Cancel this subagent"
                                    onClick=${()=>R(E.value)}>
                                Cancel subagent
                            </button>
                        `)}
                </div>
            `}
            <div id="messages" role="log" aria-live="polite" ref=${e}>
                ${ae.value.length===0&&d`
                    <div class="empty-state">
                        ${a?`No activity recorded in this session yet.`:`No messages yet. Send a message to start.`}
                    </div>
                `}
                ${t.map(e=>{if(e._isToolGroup)return d`
                            <${Ur} key=${e.key} count=${e.tools.length}>
                                ${e.tools.map(e=>d`<${Hr} key=${e.id} ...${e} />`)}
                            <//>
                        `;let t=e;if(t.type===`user`||t.type===`agent`)return d`<${$n} key=${t.id} type=${t.type} text=${t.text} sealed=${t.sealed} fromAgent=${t.fromAgent} reasoning=${t.reasoning} ts=${t.ts} />`;if(t.type===`tool`)return d`<${Hr} key=${t.id} ...${t} />`;if(t.type===`context_debug`)return d`<${Jr} key=${t.id} ...${t} />`;if(t.type===`approval`)return d`<${Xr} key=${t.id} ...${t} />`;if(t.type===`job_completed`)return d`<${ai} key=${t.id} jobName=${t.jobName} status=${t.status} summary=${t.summary} ts=${t.ts} runId=${t.runId} truncated=${t.truncated} jobSessionUuid=${t.jobSessionUuid} jobSessionId=${t.jobSessionId} />`;if(t.type===`subagent_completed`)return d`<${ui} key=${t.id}
                            name=${t.name} task=${t.task} status=${t.status}
                            toolCount=${t.toolCount} durationMs=${t.durationMs}
                            sessionId=${t.sessionId} summary=${t.summary} />`;if(t.type===`image`){let e=!!t.fromAgent,n=t.role===`user`&&!e?`user`:`agent`,r=b.value||B.value?.name,i=t.role===`user`&&!e?`>`:t.fromAgent?`${t.fromAgent} $`:r?`${r} $`:`$`;return d`
                            <div key=${t.id} class="msg ${n}">
                                <div class="msg-label-row">
                                    <div class="msg-label">${i}</div>
                                    ${t.ts&&d`<${Xn} ts=${t.ts} />`}
                                </div>
                                <div class="msg-body">
                                    ${t.url?d`<img src=${t.url} alt=${t.alt||``} style="max-width:100%;border-radius:8px;" />`:`[Image${t.alt?`: `+t.alt:``}]`}
                                    ${t.alt&&d`<div style="font-size:var(--text-xs);color:var(--text-secondary);margin-top:var(--space-2);">${t.alt}</div>`}
                                </div>
                            </div>
                        `}if(t.type===`error`)return d`<${tr} key=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`warning`)return d`<${nr} key=${t.id} id=${t.id} text=${t.text} code=${t.code} />`;if(t.type===`run_boundary`)return d`<${ir} key=${t.id} status=${t.status} error=${t.error} />`;if(t.type===`system`)return d`<${rr} key=${t.id} text=${t.text} />`;if(t.type===`dm_ended`)return d`<${ar} key=${t.id} peer=${t.peer} reason=${t.reason} />`;if(t.type===`notification`){let e=t.metadata||{};return e.type===`dm_ended_notification`?d`<${ar} key=${t.id} peer=${e.peer||`unknown`} reason=${_e[e.reason]||e.reason||`conversation ended`} />`:d`<${rr} key=${t.id} text=${t.text} />`}if(t.type===`tokens`)return d`<${er} key=${t.id} usage=${t.usage} />`;if(t.type===`thinking`){let e=`Thinking`,n=`thinking-indicator`;t.pending?(e=`Sending`,n=`pending-indicator`):t.queuedBehind>0?(e=`Queued \u2014 position ${t.queuedBehind}`,n=`queued-indicator`):t.source&&t.source.startsWith(`peer:`)?e=`Replying to message from `+t.source.slice(5):t.source===`job`?e=`Running scheduled job`:t.source===`subagent`&&(e=`Processing subagent result`);let r=B.value?.name||`Agent`;return d`
                            <div key=${t.id} class="msg agent">
                                <div class="msg-label">${r} $</div>
                                <div class="msg-body ${n}">${e}</div>
                            </div>
                        `}return null})}
            </div>
            <${fi} />
            <${co} />
            ${a?d`
                    <div class="internal-session-footer">
                        <span class="internal-session-footer-text">This is a read-only view of internal agent activity.</span>
                    </div>
                `:d`<${vi} />`}
            `}
        </div>
    `}function wo(){let e=a(!1);return d`
        <${qt} status=${xo} onOpenSettings=${()=>{e.value=!0}} />
        <${bo} />
        ${H.value.length>0?d`
                <div id="main">
                    <${An} />
                    <${Co} />
                    <${Za} />
                </div>`:d`<${oo} />`}
        <${ao} open=${e.value} onClose=${()=>{e.value=!1}} />
    `}n(d`<${wo} />`,document.getElementById(`app`));function To(){He.value=!1,xo.value=`connecting...`,Ct().then(()=>{xo.value=`connected`}).catch(()=>{xo.value=`offline`,He.value=!0})}Ge(To),we(),To();export{wt as a,Ct as i,mi as n,bt as o,pi as r,Et as s,xo as status,vi as t};