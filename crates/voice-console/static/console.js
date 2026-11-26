
    const activeBoard = document.getElementById('active-board');
    const recentBoard = document.getElementById('recent-board');
    const statusEl = document.getElementById('telephony-status');
    const refreshBtn = document.getElementById('refresh-sessions');

    function escapeHtml(value) {
      return String(value ?? '')
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;')
        .replaceAll("'", '&#39;');
    }

    function isRoleplaySession(session) {
      return String(session?.mode || '').toLowerCase() === 'roleplay'
        || String(session?.prompt_profile || '').toLowerCase() === 'roleplay'
        || String(session?.target_number || '') === '9199';
    }

    function lineLabel(target, profile, session) {
      const number = target || '—';
      if (session && isRoleplaySession(session)) {
        const label = session.roleplay_label || session.roleplay_scenario || 'practice';
        return '9199 · Roleplay · ' + label;
      }
      if (profile === 'sop_enquiries' || profile === 'sop_enquiries_realtime' || number === '9198') return number + ' · Enquiries';
      if (number === '9197') return number + ' · AI Pilot';
      if (number === '9199') return '9199 · Roleplay';
      return number;
    }

    function phaseLabel(phase, status) {
      const value = String(phase || status || '').toLowerCase();
      const map = {
        queued: 'Processing…', processing: 'Processing…', transcribed: 'Caller heard',
        reasoned: 'Reply ready', completed: 'Spoken', opening_greeting: 'Greeting',
        skipped: 'Skipped', failed: 'Failed',
      };
      return map[value] || (value ? value.replaceAll('_', ' ') : 'Idle');
    }

    function sessionPill(session) {
      const raw = String(session.status || '').toLowerCase();
      const phase = String(session.latest_phase || '').toLowerCase();
      if (raw === 'transferred') return { cls: 'transferred', label: 'Transferred' };
      if (raw === 'callback_requested') return { cls: 'callback', label: 'Callback' };
      if (raw === 'completed') return { cls: 'completed', label: 'Ended' };
      if (raw === 'failed') return { cls: 'completed', label: 'Failed' };
      if (['queued','processing','transcribed','reasoned'].includes(phase) || session.is_active) {
        return { cls: 'live', label: 'Live' };
      }
      return { cls: 'recent', label: 'Recent' };
    }

    function formatSlots(slots) {
      if (!slots) return '';
      const labels = { debt_type: 'Debt', amount: 'Amount', staff_name: 'Staff', reference: 'Ref' };
      const chips = [];
      for (const [key, label] of Object.entries(labels)) {
        const value = slots[key];
        if (value == null || value === '' || value === 'unknown' || value === 'null') continue;
        chips.push(label + ': ' + value);
      }
      return chips.join(' · ');
    }

    function progressHtml(phase) {
      const p = String(phase || '').toLowerCase();
      const order = ['queued','processing','transcribed','reasoned','completed'];
      let idx = order.indexOf(p);
      if (p === 'processing') idx = 0;
      const steps = [['queued','Queue'],['transcribed','Heard'],['reasoned','Reply'],['completed','Spoken']];
      return steps.map(([key, label]) => {
        let visual = key === 'queued' ? 0 : key === 'transcribed' ? 2 : key === 'reasoned' ? 3 : 4;
        let cls = 'step';
        if (key === 'queued' && (idx === 0 || idx === 1)) cls = 'step active';
        if (key === 'queued' && idx > 1) cls = 'step done';
        if (key === 'transcribed' && idx === 2) cls = 'step active';
        if (key === 'transcribed' && idx > 2) cls = 'step done';
        if (key === 'reasoned' && idx === 3) cls = 'step active';
        if (key === 'reasoned' && idx > 3) cls = 'step done';
        if (key === 'completed' && idx >= 4) cls = 'step done active';
        return `<div class="${cls}"><span class="dot"></span>${label}</div>`;
      }).join('');
    }

    function renderBubbles(session) {
      const turns = session.turns || [];
      const rp = isRoleplaySession(session);
      const human = rp ? 'Staff (you)' : 'Caller';
      const ai = rp ? 'AI customer' : 'AI';
      if (!turns.length) return '<div class="empty-chat">Waiting for conversation…</div>';
      return turns.map((turn) => {
        const turnPhase = phaseLabel(turn.phase, turn.status);
        let html = '';
        if (turn.intent === 'opening_greeting' || turn.intent === 'roleplay_customer_opening' || (turn.turn === 0 && turn.response && !turn.transcript)) {
          const open = rp ? 'AI customer opens' : 'AI greeting';
          return `<div class="bubble ai"><div class="bubble-meta">${open} · ${escapeHtml(turnPhase)}</div><div class="bubble-text">${escapeHtml(turn.response || '…')}</div></div>`;
        }
        if (turn.transcript) {
          html += `<div class="bubble caller"><div class="bubble-meta">${human} · turn ${turn.turn ?? '?'}</div><div class="bubble-text">${escapeHtml(turn.transcript)}</div></div>`;
        } else if (['queued','processing','transcribed','reasoned'].includes(String(turn.phase||'').toLowerCase())) {
          html += `<div class="bubble caller pending"><div class="bubble-meta">${human} · turn ${turn.turn ?? '?'}</div><div class="bubble-text muted">Listening / processing…</div></div>`;
        }
        if (turn.response) {
          html += `<div class="bubble ai"><div class="bubble-meta">${ai} · ${escapeHtml(turnPhase)}${turn.intent ? ' · ' + escapeHtml(turn.intent) : ''}</div><div class="bubble-text">${escapeHtml(turn.response)}</div></div>`;
        } else if (['queued','transcribed','reasoned','processing'].includes(String(turn.phase||'').toLowerCase())) {
          html += `<div class="bubble ai pending"><div class="bubble-meta">${ai} · ${escapeHtml(turnPhase)}</div><div class="bubble-text muted">Working on a reply…</div></div>`;
        }
        return html;
      }).join('');
    }

    function assistCardHtml(session) {
      const intent = String(session.latest_intent || '').toLowerCase();
      const slots = session.slots || {};
      const action = String(session.latest_action || slots.action || 'continue').toLowerCase();
      const debt = slots.debt_type;
      const amount = slots.amount;
      const rp = isRoleplaySession(session);
      let title = 'Assist', body = 'Listening for intent and slots…', tips = [], risk = '';
      if (rp) {
        title = 'Roleplay · you are the agent';
        body = `Scenario: ${session.roleplay_label || session.roleplay_scenario || 'practice'}. AI is the customer.`;
        tips = ['Stay in receptionist role', 'After hangup open Scorecard', 'Real transfer disabled in roleplay'];
        return `<div class="assist-card"><div class="eyebrow">Train · Roleplay</div><div class="assist-title">${escapeHtml(title)}</div><p class="assist-body">${escapeHtml(body)}</p><ul class="assist-tips">${tips.map(t=>`<li>${escapeHtml(t)}</li>`).join('')}</ul></div>`;
      }
      if (action === 'transfer' || intent.includes('transfer')) {
        title = 'Transfer ready'; body = 'Caller wants a person. Confirm details, then bridge to 1000.';
        tips = ['Confirm name / callback', 'Copy handoff brief']; risk = 'transfer requested';
      } else if (action === 'callback' || intent.includes('callback')) {
        title = 'Callback'; body = 'Caller requested a callback.'; tips = ['Note preferred time', 'Confirm number'];
      } else if (action === 'decline_debt_type' || intent.includes('unsupported')) {
        title = 'Unsupported debt type'; body = 'Direct to LOD template on slaterbyrne.com.au (Services).';
        tips = ['Do not open residential/rental/childcare/vet/gym as new files']; risk = 'unsupported debt';
      } else if (intent.includes('fee')) {
        title = 'Fees'; body = 'No joining fee · no account fee · commission on success only.';
        tips = ['Keep answer short'];
      } else if (debt && !amount) {
        title = 'Amount missing'; body = `Debt type: ${debt}. Still need amount.`; tips = ['Ask approximate amount']; risk = 'no amount';
      } else if (debt && amount) {
        title = 'Intake in progress'; body = `Debt: ${debt} · Amount: ${amount}.`; tips = ['Confirm supported type'];
      } else if (session.is_active) {
        title = 'Live enquiry'; body = 'AI is handling the call.'; tips = ['Open Capture after hangup'];
      } else return '';
      return `<div class="assist-card"><div style="display:flex;justify-content:space-between"><div class="eyebrow">Ride-along · Assist</div>${risk?`<span class="chip risk">${escapeHtml(risk)}</span>`:''}</div><div class="assist-title">${escapeHtml(title)}</div><p class="assist-body">${escapeHtml(body)}</p><ul class="assist-tips">${tips.map(t=>`<li>${escapeHtml(t)}</li>`).join('')}</ul></div>`;
    }

    function buildHandoffText(session) {
      if (session.handoff_brief && String(session.handoff_brief).trim().length > 10) return String(session.handoff_brief).trim();
      if (session.post_analysis && session.post_analysis.internal_handoff_brief) return String(session.post_analysis.internal_handoff_brief).trim();
      const slots = session.slots || {};
      return [
        'INTERNAL HANDOFF BRIEF',
        `Call: ${session.session_id || '—'}`,
        `Line: ${session.target_number || '—'}`,
        `Status: ${session.status || ''} · Action: ${session.latest_action || slots.action || 'continue'}`,
        `Intent: ${session.latest_intent || '—'}`,
        `Debt type: ${slots.debt_type || '—'}`,
        `Amount: ${slots.amount || '—'}`,
        `Caller last said: ${session.latest_transcript || '—'}`,
        `AI last said: ${session.latest_response || '—'}`,
        'Please greet the caller and continue intake.',
      ].join('\n');
    }

    function visualKey(sessions) {
      return (sessions || []).map((s) => [
        s.session_id, s.is_active ? '1' : '0', s.status || '', s.latest_phase || '',
        s.latest_transcript || '', s.latest_response || '', s.latest_action || '',
        s.latest_intent || '', s.turn_count ?? 0, formatSlots(s.slots),
        ...(s.turns || []).map((t) => [t.turn, t.phase, t.status, t.transcript, t.response, t.intent].join('\x1e')),
      ].join('|')).join('\n');
    }

    function renderSessionCard(session) {
      const pill = sessionPill(session);
      const line = lineLabel(session.target_number, session.prompt_profile, session);
      const phase = phaseLabel(session.latest_phase, session.status);
      const slots = formatSlots(session.slots);
      const latency = session.latest_latency_ms != null ? (session.latest_latency_ms / 1000).toFixed(1) + 's' : '';
      const assist = assistCardHtml(session);
      const rp = isRoleplaySession(session);
      return `
        <article class="call-card" data-session-id="${escapeHtml(session.session_id)}" data-active="${session.is_active ? 'true' : 'false'}">
          <div class="call-top">
            <div>
              <div class="line-title">${escapeHtml(line)}</div>
              <div class="phase-line">${escapeHtml(phase)}${latency ? ' · ' + latency : ''}</div>
            </div>
            <span class="pill pill-${pill.cls}">${escapeHtml(pill.label)}</span>
          </div>
          <div class="progress-track">${progressHtml(session.latest_phase)}</div>
          <div class="now-grid">
            <div class="now-block">
              <div class="eyebrow">${rp ? 'You just said' : 'Caller just said'}</div>
              <p>${escapeHtml(session.latest_transcript || '—')}</p>
            </div>
            <div class="now-block accent">
              <div class="eyebrow">${rp ? 'Customer (AI) said' : 'AI reply'}</div>
              <p>${escapeHtml(session.latest_response || '—')}</p>
            </div>
          </div>
          ${slots ? `<div class="slot-row">${escapeHtml(slots)}</div>` : ''}
          ${assist}
          <div class="chat-feed">${renderBubbles(session)}</div>
          <div class="card-actions">
            <a class="btn summary-btn" href="${P('/summary/' + encodeURIComponent(session.session_id))}">${rp ? 'Scorecard' : 'Summary Inform'}</a>
            <button type="button" class="secondary copy-handoff" data-session-id="${escapeHtml(session.session_id)}">${rp ? 'Copy coach notes' : 'Copy handoff brief'}</button>
          </div>
          <details class="tech-details">
            <summary>Technical details</summary>
            <p class="meta">session=${escapeHtml(String(session.session_id || '').slice(0,16))}… · turns=${session.turn_count} · action=${escapeHtml(session.latest_action || 'continue')} · status=${escapeHtml(session.status || '')}</p>
          </details>
        </article>`;
    }

    const emptyLive = `<article class="call-card empty-state" data-empty="1"><h3 style="margin:0 0 8px;color:var(--ink)">No live call right now</h3><p class="muted">Dial <code>9198</code> or roleplay <code>9199</code>.</p></article>`;
    const emptyRecent = `<article class="call-card empty-state" data-empty="1"><p class="muted">No recent calls yet.</p></article>`;

    function syncBoard(board, sessions, emptyHtml) {
      if (!sessions.length) {
        if (!board.querySelector('[data-empty="1"]')) board.innerHTML = emptyHtml;
        return;
      }
      if (board.querySelector('[data-empty="1"]')) board.innerHTML = '';
      const existing = new Map();
      board.querySelectorAll('.call-card[data-session-id]').forEach((el) => existing.set(el.dataset.sessionId, el));
      const keep = new Set();
      sessions.forEach((session, index) => {
        const id = String(session.session_id || '');
        keep.add(id);
        const nextHtml = renderSessionCard(session).trim();
        let card = existing.get(id);
        if (!card) {
          board.insertAdjacentHTML('beforeend', nextHtml);
          card = board.querySelector(`.call-card[data-session-id="${CSS.escape(id)}"]`);
          const feed = card?.querySelector('.chat-feed');
          if (feed) feed.scrollTop = feed.scrollHeight;
        } else {
          const feed = card.querySelector('.chat-feed');
          const gap = feed ? feed.scrollHeight - feed.scrollTop - feed.clientHeight : 0;
          const stick = !feed || gap < 72;
          const prevTop = feed ? feed.scrollTop : 0;
          const prevKey = card.getAttribute('data-visual-key') || '';
          const nextKey = visualKey([session]);
          if (prevKey !== nextKey) {
            const wrap = document.createElement('div');
            wrap.innerHTML = nextHtml;
            const newCard = wrap.firstElementChild;
            newCard.setAttribute('data-visual-key', nextKey);
            card.replaceWith(newCard);
            card = newCard;
            const newFeed = card.querySelector('.chat-feed');
            if (newFeed) {
              if (stick) newFeed.scrollTop = newFeed.scrollHeight;
              else newFeed.scrollTop = prevTop;
            }
          }
        }
        if (card) {
          card.setAttribute('data-visual-key', visualKey([session]));
          const current = board.children[index];
          if (current !== card) board.insertBefore(card, current || null);
        }
      });
      existing.forEach((el, id) => { if (!keep.has(id)) el.remove(); });
    }

    let lastKey = '';
    function applySessions(payload, source) {
      const key = visualKey(payload);
      if (key === lastKey && source !== 'manual') return;
      lastKey = key;
      const active = (payload || []).filter((s) => s.is_active);
      const recent = (payload || []).filter((s) => !s.is_active).slice(0, 6);
      syncBoard(activeBoard, active, emptyLive);
      syncBoard(recentBoard, recent, emptyRecent);
      const n = (payload || []).length;
      statusEl.textContent = `Live feed · ${active.length} active · ${n} sessions · ${source}`;
    }

    async function refreshOnce() {
      try {
        const response = await fetch(P('/api/telephony/sessions?limit=10'), { cache: 'no-store' });
        const payload = await response.json();
        applySessions(payload, 'manual');
      } catch (e) {
        statusEl.textContent = 'Refresh failed — retrying…';
      }
    }

    function connectEvents() {
      const source = new EventSource(P('/api/telephony/events?limit=10'));
      source.addEventListener('sessions', (ev) => {
        try {
          applySessions(JSON.parse(ev.data), 'sse');
        } catch (_e) { /* ignore */ }
      });
      source.addEventListener('error', () => {
        statusEl.textContent = 'Live feed reconnecting…';
      });
      source.onopen = () => { statusEl.textContent = 'Live feed connected'; };
    }

    document.addEventListener('click', async (event) => {
      const btn = event.target.closest('.copy-handoff');
      if (!btn) return;
      const sid = btn.getAttribute('data-session-id');
      let text = '';
      try {
        const sessions = await (await fetch(P('/api/telephony/sessions?limit=20'), { cache: 'no-store' })).json();
        const found = (sessions || []).find((s) => String(s.session_id) === String(sid));
        if (found) text = buildHandoffText(found);
        if (!text || text.length < 20) {
          const res = await fetch(P('/api/telephony/sessions/' + encodeURIComponent(sid) + '/summary?force=false'), { cache: 'no-store' });
          if (res.ok) {
            const payload = await res.json();
            if (payload.handoff_brief) text = payload.handoff_brief;
            else if (payload.analysis && payload.analysis.internal_handoff_brief) text = payload.analysis.internal_handoff_brief;
          }
        }
      } catch (_e) { /* ignore */ }
      if (!text) text = 'No brief available yet.';
      try {
        await navigator.clipboard.writeText(text);
        const prev = btn.textContent;
        btn.textContent = 'Copied!';
        setTimeout(() => { btn.textContent = prev; }, 1400);
      } catch (_e) {
        btn.textContent = 'Failed';
      }
    });

    const form = document.getElementById('upload-form');
    if (form) {
      form.addEventListener('submit', async (event) => {
        event.preventDefault();
        const file = document.getElementById('audio-file').files[0];
        const out = document.getElementById('upload-result');
        if (!file) return;
        const data = new FormData();
        data.append('file', file);
        out.textContent = 'Processing…';
        try {
          const response = await fetch(P('/api/device/upload'), { method: 'POST', body: data });
          const payload = await response.json();
          out.textContent = JSON.stringify(payload, null, 2);
        } catch (e) {
          out.textContent = String(e);
        }
      });
    }

    if (refreshBtn) refreshBtn.addEventListener('click', refreshOnce);
    connectEvents();
    refreshOnce();
    