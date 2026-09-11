'use strict';
const $ = id => document.getElementById(id);
const statuses = ['taken', 'waiting', 'blocked', 'lost', 'unknown'];
let offset = 0, busy = false, selected = '', before = 0, oldest = 0;
let rows = new Map();
function node(tag, text, cls) {
  const el = document.createElement(tag);
  if (text !== undefined) el.textContent = String(text);
  if (cls) el.className = cls;
  return el;
}
function badge(status) {
  const allowed = [...statuses, 'queued', 'submitted', 'not_delivered', 'uncertain', 'discarded', 'running', 'completed', 'failed'];
  return node('span', status, 'badge ' + (allowed.includes(status) ? status : ''));
}
async function api(path) {
  const response = await fetch(path, {cache: 'no-store', signal: AbortSignal.timeout(6000)});
  if (!response.ok) throw new Error('Refresh unavailable (HTTP ' + response.status + '). Retrying automatically.');
  return response.json();
}
function counts() {
  $('summary').replaceChildren(...statuses.map(status => {
    const box = node('div', undefined, 'metric');
    const count = [...rows.values()].filter(r => r.status === status).length;
    box.append(node('strong', count), node('span', status + ' · this page'));
    return box;
  }));
}
function renderDeliveries(data) {
  const next = new Map();
  for (const delivery of data.deliveries) {
    const id = delivery.request_id;
    let row = rows.get(id);
    if (!row) {
      const tr = node('tr');
      const identity = node('td', id);
      identity.append(node('small', delivery.target));
      const sender = node('td'); const receipt = node('td', 'Checking…'); const reason = node('td', 'Reading receipt evidence…');
      tr.append(identity, sender, receipt, reason);
      row = {tr, sender, receipt, reason, status: 'checking'};
    }
    row.sender.replaceChildren(badge(delivery.status));
    next.set(id, row);
  }
  rows = next;
  $('deliveries').replaceChildren(...[...rows.values()].map(row => row.tr));
  $('delivery-empty').hidden = data.deliveries.length !== 0;
  $('delivery-count').textContent = data.delivery_count + ' total';
  counts();
}
async function receipts() {
  const pending = [...rows.entries()];
  async function worker() {
    while (pending.length) {
      const [id, row] = pending.shift();
      let receipt;
      try { receipt = await api('/api/receipt?id=' + encodeURIComponent(id)); }
      catch (e) { receipt = {status:'unknown', reason: e.message}; }
      row.status = statuses.includes(receipt.status) ? receipt.status : 'unknown';
      row.receipt.replaceChildren(badge(row.status));
      row.reason.textContent = receipt.reason || 'No evidence description available.';
      counts();
    }
  }
  await Promise.all(Array.from({length: 4}, worker));
}
function renderJobs(data) {
  $('job-count').textContent = data.job_count + ' total';
  $('jobs').replaceChildren(...data.jobs.map(job => {
    const el = node('article', undefined, 'job');
    const heading = node('div', undefined, 'job-heading');
    heading.append(node('strong', job.agent), badge(job.status));
    el.append(heading, node('small', job.id), node('p', 'Room: ' + job.room));
    if (job.error) el.append(node('p', job.error));
    return el;
  }));
  if (!data.jobs.length) $('jobs').append(node('p', 'No jobs on this page.', 'empty'));
}
function renderRooms(data) {
  const choices = data.rooms.map(room => {
    const option = node('option', room.room + ' · ' + room.count + ' messages');
    option.value = room.room; return option;
  });
  if (!data.rooms.some(room => room.room === selected)) {
    selected = data.rooms[0]?.room || ''; before = 0;
  }
  $('room').replaceChildren(...choices); $('room').value = selected;
  $('room').disabled = !choices.length;
}
async function messages() {
  const room = selected, cursor = before;
  if (!room) { $('messages').replaceChildren(node('p', 'No room messages yet.', 'empty')); return; }
  try {
    const data = await api('/api/messages?room=' + encodeURIComponent(room) + '&before=' + cursor);
    if (room !== selected || cursor !== before) return;
    oldest = data.messages[0]?.id || 0;
    $('older').disabled = !data.has_older;
    $('room-status').textContent = cursor ? 'History page · updates paused here' : 'Latest 100 · auto-updating';
    $('messages').replaceChildren(...data.messages.map(message => {
      const el = node('article', undefined, 'message');
      const heading = node('header'); heading.append(node('strong', message.sender), node('span', '#' + message.id));
      el.append(heading, node('pre', message.text)); return el;
    }));
  } catch (e) { $('room-status').textContent = e.message; }
}
async function refresh() {
  if (busy) return;
  busy = true;
  try {
    const data = await api('/api/overview?offset=' + offset);
    renderDeliveries(data); renderJobs(data); renderRooms(data);
    $('page').textContent = 'Records page ' + (offset / data.page_size + 1);
    $('previous').disabled = offset === 0;
    $('next').disabled = offset + data.page_size >= Math.max(data.delivery_count, data.job_count);
    $('error').hidden = true;
    await Promise.all([receipts(), messages()]);
    $('updated').textContent = 'Updated ' + new Date().toLocaleTimeString();
  } catch (e) {
    $('error').textContent = e.message + ' Displayed records may be stale.';
    $('error').hidden = false;
    $('updated').textContent = 'Update failed · retrying';
  } finally { busy = false; }
}
$('room').addEventListener('change', () => { selected = $('room').value; before = 0; messages(); });
$('older').addEventListener('click', () => { before = oldest; messages(); });
$('latest').addEventListener('click', () => { before = 0; messages(); });
$('previous').addEventListener('click', () => { if (!busy) { offset = Math.max(0, offset - 100); refresh(); } });
$('next').addEventListener('click', () => { if (!busy) { offset += 100; refresh(); } });
refresh(); setInterval(refresh, 4000);
