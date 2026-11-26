
const state = {
  clients: [],
  clientId: "",
  kind: "closed",
  voiceId: "",
  voiceName: "",
  packVoiceId: "eve",
  lastMessage: null,
};

const $ = (id) => document.getElementById(id);

function showBanner(kind, text) {
  const el = $("brand-banner");
  el.className = "banner show " + kind;
  el.textContent = text;
}

function clearBanner() {
  const el = $("brand-banner");
  el.className = "banner";
  el.textContent = "";
}

function selectedClient() {
  return state.clients.find((c) => c.id === state.clientId) || null;
}

function datesVisible() {
  return state.kind === "closed" || state.kind === "holiday";
}

function syncKindUi() {
  document.querySelectorAll(".kind-btn").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.kind === state.kind);
  });
  $("dates-row").style.display = datesVisible() ? "flex" : "none";
}

function syncSpeedLabel() {
  const el = $("speed-value");
  if (!el) return;
  const value = Number($("speed").value);
  el.textContent = value.toFixed(2) + "×";
}

function languageLabel(code) {
  return LANG_LABELS[code] || code || "English";
}

function syncAccentFromPack() {
  const select = $("accent");
  if (!select) return;
  const client = selectedClient();
  const packLang = (client && client.language) || "en";
  const match = Array.from(select.options).find((opt) => opt.value.toLowerCase() === String(packLang).toLowerCase());
  select.value = match ? match.value : "en";
}

function esc(value) {
  return String(value || "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[ch]));
}

function optionHtml(client) {
  return `<option value="${esc(client.id)}">${esc(client.display_name)}</option>`;
}

function voiceCard(voice, packVoiceId) {
  const rec = voice.voice_id === packVoiceId ? " · pack default" : "";
  const kind = voice.kind === "cloned" ? "Cloned" : "Stock";
  const desc = voice.description ? ` · ${voice.description}` : "";
  const active = voice.voice_id === state.voiceId ? " active" : "";
  return `<button type="button" class="voice-card${active}" data-voice-id="${esc(voice.voice_id)}" data-voice-name="${esc(voice.name)}">
    <strong>${esc(voice.name)}</strong>
    <span>${kind}${rec}${esc(desc)}</span>
  </button>`;
}

async function readError(res) {
  try {
    const data = await res.json();
    if (typeof data.detail === "string") return data.detail;
    if (Array.isArray(data.detail)) return data.detail.map((d) => d.msg || JSON.stringify(d)).join("; ");
    return data.message || JSON.stringify(data);
  } catch (e) {
    return res.statusText || "Request failed";
  }
}

async function loadClients() {
  const res = await fetch(API + "/clients");
  if (!res.ok) throw new Error(await readError(res));
  const data = await res.json();
  state.clients = data.clients || [];
  state.clientId = data.default_client_id || (state.clients[0] && state.clients[0].id) || "";
  $("client-select").innerHTML = state.clients.map(optionHtml).join("") || '<option value="">No client packs</option>';
  $("client-select").value = state.clientId;
}

async function loadVoices() {
  if (!state.clientId) return;
  const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/voices");
  if (!res.ok) throw new Error(await readError(res));
  const data = await res.json();
  state.packVoiceId = data.pack_voice_id || "eve";
  const voices = [].concat(data.stock || [], data.cloned || []);
  if (!state.voiceId || !voices.some((v) => v.voice_id === state.voiceId)) {
    state.voiceId = state.packVoiceId || (voices[0] && voices[0].voice_id) || "eve";
    const match = voices.find((v) => v.voice_id === state.voiceId);
    state.voiceName = match ? match.name : state.voiceId;
  }
  const stockHtml = (data.stock || []).map((v) => voiceCard(v, state.packVoiceId)).join("");
  const clonedHtml = (data.cloned || []).map((v) => voiceCard(v, state.packVoiceId)).join("");
  $("stock-voices").innerHTML = stockHtml || '<p class="hint">No stock voices returned.</p>';
  $("cloned-voices").innerHTML = clonedHtml || '<p class="hint">No cloned voices yet. Upload a sample or paste a Voice ID.</p>';
  if (data.xai_error) {
    showBanner("error", "Voice list warning: " + data.xai_error);
  }
}

async function loadLibrary() {
  if (!state.clientId) return;
  const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/messages");
  if (!res.ok) throw new Error(await readError(res));
  const data = await res.json();
  const rows = (data.messages || []).map((m) => {
    const when = (m.starts_on && m.ends_on) ? `${m.starts_on} → ${m.ends_on}` : "—";
    const audio = API + "/" + encodeURIComponent(m.client_id) + "/messages/" + encodeURIComponent(m.id) + "/audio";
    const dl = audio + "?download=1";
    const snippet = (m.script || "").slice(0, 160) + ((m.script || "").length > 160 ? "…" : "");
    return `<tr>
      <td>
        <div style="font-weight:600">${esc(m.title)}</div>
        <div class="meta">${esc(m.kind.replace("_", " "))} · ${esc(m.voice_name || m.voice_id)} · ${esc(languageLabel(m.language))} · ${Number(m.speed || 1).toFixed(2)}×</div>
        <div class="library-script">${esc(snippet)}</div>
      </td>
      <td>${esc(when)}</td>
      <td>
        <audio controls preload="none" src="${audio}"></audio>
        <div class="card-actions" style="margin-top:8px">
          <a class="btn secondary" href="${dl}">Download</a>
          <button type="button" class="btn-danger js-delete" data-id="${m.id}">Delete</button>
        </div>
      </td>
    </tr>`;
  }).join("");
  $("library-body").innerHTML = rows || '<tr><td colspan="3"><div class="empty-box">No brand messages yet. Generate one to save and download it.</div></td></tr>';
}

async function fillTemplate() {
  if (!state.clientId) return;
  const params = new URLSearchParams({
    kind: state.kind,
    starts_on: $("starts-on").value,
    ends_on: $("ends-on").value,
  });
  const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/template?" + params);
  if (!res.ok) throw new Error(await readError(res));
  const data = await res.json();
  $("script").value = data.script || "";
  if (!$("title").value) $("title").value = data.title || "";
}

async function generateMessage() {
  clearBanner();
  const btn = $("generate-btn");
  btn.disabled = true;
  btn.textContent = "Generating…";
  try {
    const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/messages", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        title: $("title").value,
        kind: state.kind,
        script: $("script").value,
        voice_id: state.voiceId,
        voice_name: state.voiceName,
        language: $("accent").value,
        speed: Number($("speed").value),
        starts_on: $("starts-on").value,
        ends_on: $("ends-on").value,
      }),
    });
    if (!res.ok) throw new Error(await readError(res));
    const data = await res.json();
    state.lastMessage = data.message;
    $("preview-wrap").style.display = "block";
    $("preview-audio").src = data.audio_url;
    $("preview-download").href = data.download_url;
    $("preview-download").setAttribute("download", data.message.download_name || "brand-message.wav");
    showBanner("ok", "Message saved. Preview below — use Download to keep the WAV.");
    await $("preview-audio").play().catch(() => {});
    await loadLibrary();
  } catch (err) {
    showBanner("error", err.message || String(err));
  } finally {
    btn.disabled = false;
    btn.textContent = "Generate, save & preview";
  }
}

async function cloneVoice(ev) {
  ev.preventDefault();
  clearBanner();
  const file = $("clone-file").files[0];
  if (!file) {
    showBanner("error", "Choose a voice sample first.");
    return;
  }
  const btn = $("clone-btn");
  btn.disabled = true;
  btn.textContent = "Cloning…";
  try {
    const form = new FormData();
    form.append("file", file);
    form.append("name", $("clone-name").value || file.name);
    form.append("gender", $("clone-gender").value);
    form.append("tone", $("clone-tone").value);
    const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/clone", {
      method: "POST",
      body: form,
    });
    const data = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(data.detail || "Clone failed");
    state.voiceId = data.voice.voice_id;
    state.voiceName = data.voice.name;
    showBanner("ok", "Cloned voice ready: " + data.voice.name);
    await loadVoices();
  } catch (err) {
    showBanner("error", err.message || String(err));
  } finally {
    btn.disabled = false;
    btn.textContent = "Clone from sample";
  }
}

async function registerVoice(ev) {
  ev.preventDefault();
  clearBanner();
  try {
    const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/voices", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        voice_id: $("import-id").value,
        name: $("import-name").value,
      }),
    });
    const data = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(data.detail || "Could not register voice");
    state.voiceId = data.voice.voice_id;
    state.voiceName = data.voice.name;
    showBanner("ok", "Voice ID registered: " + data.voice.voice_id);
    $("import-id").value = "";
    await loadVoices();
  } catch (err) {
    showBanner("error", err.message || String(err));
  }
}

async function deleteMessage(id) {
  if (!confirm("Delete this brand message and its audio?")) return;
  const res = await fetch(API + "/" + encodeURIComponent(state.clientId) + "/messages/" + encodeURIComponent(id), {
    method: "DELETE",
  });
  if (!res.ok) {
    showBanner("error", await readError(res));
    return;
  }
  await loadLibrary();
}

document.addEventListener("click", (event) => {
  const kindBtn = event.target.closest(".kind-btn");
  if (kindBtn) {
    state.kind = kindBtn.dataset.kind;
    syncKindUi();
    return;
  }
  const voiceBtn = event.target.closest(".voice-card");
  if (voiceBtn) {
    state.voiceId = voiceBtn.dataset.voiceId;
    state.voiceName = voiceBtn.dataset.voiceName;
    document.querySelectorAll(".voice-card").forEach((el) => {
      el.classList.toggle("active", el === voiceBtn);
    });
    return;
  }
  const del = event.target.closest(".js-delete");
  if (del) deleteMessage(del.dataset.id);
});

$("client-select").addEventListener("change", async () => {
  state.clientId = $("client-select").value;
  state.voiceId = "";
  syncAccentFromPack();
  await loadVoices();
  await loadLibrary();
});
$("speed").addEventListener("input", syncSpeedLabel);
$("fill-btn").addEventListener("click", () => fillTemplate().catch((err) => showBanner("error", err.message)));
$("generate-btn").addEventListener("click", generateMessage);
$("clone-form").addEventListener("submit", cloneVoice);
$("import-form").addEventListener("submit", registerVoice);

(async function init() {
  try {
    syncKindUi();
    syncSpeedLabel();
    await loadClients();
    syncAccentFromPack();
    await loadVoices();
    await loadLibrary();
    await fillTemplate();
  } catch (err) {
    showBanner("error", err.message || String(err));
  }
})();
