'use strict';

const list = document.querySelector('#station-list');
const tpl = document.querySelector('#row-template');
const msg = document.querySelector('#settings-msg');
const btnSave = document.querySelector('#btn-save');
const btnRevert = document.querySelector('#btn-revert');
const proxyInput = document.querySelector('#proxy-url');
const autoplayInput = document.querySelector('#autoplay');
let saved = {proxy: '', autoplay: true, stations: []};

function setMsg(text, kind) {
	msg.textContent = text;
	msg.className = kind || '';
}

function rows() {
	return [...list.querySelectorAll('.station-row')];
}

function current() {
	return {
		proxy: proxyInput.value.trim(),
		autoplay: autoplayInput.checked,
		stations: rows().map(r => ({
			name: r.querySelector('.f-name').value.trim(),
			url: r.querySelector('.f-url').value.trim(),
			useProxy: r.querySelector('.f-proxy').checked,
		})),
	};
}

function isDirty() {
	return JSON.stringify(current()) !== JSON.stringify(saved);
}

function refresh() {
	const rs = rows();
	rs.forEach((r, i) => {
		r.querySelector('.b-drag').disabled = rs.length === 1;
		r.querySelector('.b-del').disabled = rs.length === 1;
	});
	const dirty = isDirty();
	btnSave.disabled = !dirty;
	btnRevert.disabled = !dirty;
	if (dirty) setMsg('Unsaved changes', 'dirty');
	else if (msg.className === 'dirty') setMsg('');
}

function addRow(st, focus) {
	const r = tpl.content.firstElementChild.cloneNode(true);
	r.querySelector('.f-name').value = st.name;
	r.querySelector('.f-url').value = st.url;
	r.querySelector('.f-proxy').checked = !!st.useProxy;
	list.appendChild(r);
	if (focus) r.querySelector('.f-name').focus();
	return r;
}

function render(cfg) {
	proxyInput.value = cfg.proxy || '';
	autoplayInput.checked = cfg.autoplay !== false;
	list.replaceChildren();
	cfg.stations.forEach(st => addRow(st));
	refresh();
}

// Returns a human-readable problem with a proxy URL, or '' if it looks fine.
function proxyProblem(v) {
	const m = /^([a-z0-9]+):\/\/(.*)$/i.exec(v);
	if (!m) return 'Proxy must start with socks5://, socks5h://, http:// or https://';
	if (!/^(socks5h?|https?)$/i.test(m[1])) return `Proxy type "${m[1]}" is not supported; use socks5, socks5h, http or https`;
	const hostPart = m[2].slice(m[2].lastIndexOf('@') + 1).replace(/\/$/, '');
	if (!hostPart) return 'Proxy is missing the host, e.g. socks5h://192.168.1.10:1080';
	if (/\s/.test(v)) return 'Proxy must not contain spaces';
	const port = /:(\d*)$/.exec(hostPart);
	if (port && (!port[1] || +port[1] < 1 || +port[1] > 65535)) return 'Proxy port must be a number between 1 and 65535';
	return '';
}

async function api(method, path, body) {
	// Build from origin so it also works when the page URL contains user:pass@.
	const res = await fetch(new URL('/settings/api/' + path, location.origin), {
		method,
		headers: body ? {'Content-Type': 'application/json'} : {},
		body: body ? JSON.stringify(body) : undefined,
	});
	const data = await res.json().catch(() => ({}));
	if (!res.ok) throw new Error(data.error || res.statusText);
	return data;
}

async function testRow(r, forceProxy) {
	const out = r.querySelector('.test-result');
	const url = r.querySelector('.f-url').value.trim();
	const useProxy = forceProxy ?? r.querySelector('.f-proxy').checked;
	if (!url) { out.className = 'test-result bad'; out.textContent = 'No URL'; return false; }
	out.className = 'test-result busy';
	out.textContent = useProxy ? 'testing via proxy…' : 'testing…';
	try {
		const res = await api('POST', 'check', {url, useProxy, proxy: proxyInput.value.trim()});
		const info = [res.contentType, res.bitrate && res.bitrate + ' kbps', res.icyName].filter(Boolean).join(' · ');
		out.className = 'test-result ' + (res.ok ? 'ok' : 'bad');
		out.textContent = (res.ok ? '✓ works' : '✗ ' + res.error) + (useProxy ? ' [via proxy]' : '') + (info ? '  (' + info + ')' : '');
		return res.ok;
	} catch (e) {
		out.className = 'test-result bad';
		out.textContent = '✗ ' + e.message;
		return false;
	}
}

list.addEventListener('input', e => {
	e.target.classList.remove('invalid');
	const out = e.target.closest('.station-row').querySelector('.test-result');
	if (e.target.classList.contains('f-url') || e.target.classList.contains('f-proxy')) { out.textContent = ''; out.className = 'test-result'; }
	refresh();
});
proxyInput.addEventListener('input', () => { proxyInput.classList.remove('invalid'); refresh(); });
autoplayInput.addEventListener('change', refresh);

document.querySelector('#btn-proxy-failing').addEventListener('click', async e => {
	if (!proxyInput.value.trim()) {
		setMsg('Enter a proxy first', 'bad');
		proxyInput.focus();
		return;
	}
	e.target.disabled = true;
	setMsg('Testing direct connections…');
	const rs = rows();
	const direct = await Promise.all(rs.map(r => testRow(r, false)));
	const failing = rs.filter((r, i) => !direct[i]);
	setMsg(`Testing ${failing.length} failing stations via proxy…`);
	const viaProxy = await Promise.all(failing.map(r => testRow(r, true)));
	let fixed = 0;
	failing.forEach((r, i) => { if (viaProxy[i]) { r.querySelector('.f-proxy').checked = true; fixed++; } });
	// Stations that work directly don't need the proxy.
	rs.forEach((r, i) => { if (direct[i]) r.querySelector('.f-proxy').checked = false; });
	refresh();
	const still = failing.length - fixed;
	setMsg(`${fixed} station(s) work via proxy and were ticked` + (still ? `, ${still} fail either way` : '') + '. Review and Save.', still ? 'dirty' : 'ok');
	e.target.disabled = false;
});

list.addEventListener('click', e => {
	const b = e.target.closest('button');
	if (!b) return;
	const r = b.closest('.station-row');
	if (b.classList.contains('b-del')) r.remove();
	if (b.classList.contains('b-test')) { testRow(r); return; }
	refresh();
	if (document.body.contains(b) && !b.disabled) b.focus();
});

// Reordering: drag a row by its handle (mouse, pen or touch); with the
// handle focused, the arrow keys move the row too.
let drag = null;
list.addEventListener('pointerdown', e => {
	const h = e.target.closest('.b-drag');
	if (!h || h.disabled || e.button !== 0) return;
	e.preventDefault();
	h.setPointerCapture(e.pointerId);
	drag = {row: h.closest('.station-row'), handle: h, id: e.pointerId};
	drag.row.classList.add('dragging');
	list.classList.add('drag-active');
});
list.addEventListener('pointermove', e => {
	if (!drag || e.pointerId !== drag.id) return;
	const y = e.clientY;
	// Move past a neighbour once the pointer crosses its middle.
	const prev = drag.row.previousElementSibling, next = drag.row.nextElementSibling;
	if (prev && y < prev.getBoundingClientRect().top + prev.offsetHeight / 2) list.insertBefore(drag.row, prev);
	else if (next && y > next.getBoundingClientRect().top + next.offsetHeight / 2) list.insertBefore(next, drag.row);
	else return;
	// Keep the dragged row in view near the window edges.
	const rect = drag.row.getBoundingClientRect();
	if (rect.top < 40 || rect.bottom > innerHeight - 40) drag.row.scrollIntoView({block: 'nearest'});
});
function endDrag(e) {
	if (!drag || e.pointerId !== drag.id) return;
	drag.row.classList.remove('dragging');
	list.classList.remove('drag-active');
	drag = null;
	refresh();
}
list.addEventListener('pointerup', endDrag);
list.addEventListener('pointercancel', endDrag);
list.addEventListener('keydown', e => {
	const h = e.target.closest('.b-drag');
	if (!h || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
	e.preventDefault();
	const r = h.closest('.station-row');
	if (e.key === 'ArrowUp' && r.previousElementSibling) list.insertBefore(r, r.previousElementSibling);
	if (e.key === 'ArrowDown' && r.nextElementSibling) list.insertBefore(r.nextElementSibling, r);
	h.focus();
	refresh();
});

document.querySelector('#btn-add').addEventListener('click', () => {
	addRow({name: '', url: ''}, true);
	refresh();
});

document.querySelector('#btn-test-all').addEventListener('click', async e => {
	if (!rows().length) return;
	e.target.disabled = true;
	const results = await Promise.all(rows().map(r => testRow(r)));
	const bad = results.filter(ok => !ok).length;
	setMsg(bad ? `${bad} of ${results.length} stations not working` : `All ${results.length} stations work`, bad ? 'bad' : 'ok');
	e.target.disabled = false;
});

btnRevert.addEventListener('click', () => { render(saved); setMsg(''); });

btnSave.addEventListener('click', async () => {
	let firstBad = null;
	for (const r of rows()) {
		for (const cls of ['.f-name', '.f-url']) {
			const inp = r.querySelector(cls);
			const v = inp.value.trim();
			const bad = cls === '.f-name' ? !v : !/^https?:\/\/\S+$/i.test(v);
			inp.classList.toggle('invalid', bad);
			if (bad && !firstBad) firstBad = inp;
		}
	}
	const pv = proxyInput.value.trim();
	const proxyErr = pv ? proxyProblem(pv) : '';
	if (proxyErr) {
		proxyInput.classList.add('invalid');
		setMsg(proxyErr, 'bad');
		proxyInput.focus();
		return;
	}
	if (rows().some(r => r.querySelector('.f-proxy').checked) && !pv) {
		setMsg('Some stations use the proxy, but no proxy is set', 'bad');
		proxyInput.classList.add('invalid');
		proxyInput.focus();
		return;
	}
	if (firstBad) {
		setMsg('Fill in every name and an http(s) URL', 'bad');
		firstBad.focus();
		return;
	}
	btnSave.disabled = true;
	setMsg('Saving…');
	try {
		saved = await api('PUT', 'stations', current());
		render(saved);
		setMsg(`Saved ${saved.stations.length} stations. Listeners get the new list right away.`, 'ok');
	} catch (e) {
		setMsg('Not saved: ' + e.message, 'bad');
		refresh();
	}
});

window.addEventListener('beforeunload', e => {
	if (isDirty()) { e.preventDefault(); e.returnValue = ''; }
});

api('GET', 'stations')
	.then(st => { saved = st; render(st); })
	.catch(e => setMsg('Could not load stations: ' + e.message, 'bad'));
