// Register the service worker so the app is installable as a PWA.
if ('serviceWorker' in navigator) {
	window.addEventListener('load', () => {
		navigator.serviceWorker.register('/sw.js').catch(err => {
			console.warn('Service worker registration failed:', err);
		});
	});
}

const status = document.querySelector('#status');

// Htmx:wsConnecting
// htmx:wsError

let socket;
let elt;

// Document.addEventListener('visibilitychange', event => {
//	console.log('visibilitychange', document.visibilityState);
//	if (socket) {
//		socket.send(document.visibilityState, elt);
//	}
// });
//
document.body.addEventListener('htmx:wsClose', event => {
	console.log('disconnected');
	status.innerText = 'Disconnected';
	status.dataset.status = 'disconnected';
});

document.body.addEventListener('htmx:wsOpen', event => {
	console.log('connected');

	socket = event.detail.socketWrapper;
	elt = event.detail.elt;

	status.innerText = 'Connected';
	status.dataset.status = 'connected';
});

document.body.addEventListener('htmx:wsClose', event => {
	console.log('disconnected');
	status.innerText = 'Disconnected';
	status.dataset.status = 'disconnected';
});

const player = document.querySelector('#audio-player');
// Settings page "Autoplay": start playing as soon as the page opens.
const AUTOPLAY = player.hasAttribute('data-autoplay');

// Workaround to prevent player from playing when replaced
let audioSource = document.querySelector('#audio-source');
let currentSource = audioSource.src;
let playAfterSwitch = false;

// Start playback. Browsers block sound until the visitor has interacted with
// the page, so if this is refused, start on the first click, tap or key.
const GESTURE_EVENTS = ['pointerdown', 'keydown', 'touchstart'];
let waitingForGesture = false;
function startPlayback() {
	player.play().catch(err => {
		if (err && err.name === 'NotAllowedError') startOnFirstGesture();
	});
}
function startOnFirstGesture() {
	if (waitingForGesture) return;
	waitingForGesture = true;
	document.body.classList.add('needs-gesture');
	status.innerText = 'Click anywhere to start playing';
	const go = () => {
		for (const ev of GESTURE_EVENTS) document.removeEventListener(ev, go, true);
		waitingForGesture = false;
		document.body.classList.remove('needs-gesture');
		status.innerText = socket ? 'Connected' : '';
		player.play().catch(() => {});
	};
	for (const ev of GESTURE_EVENTS) document.addEventListener(ev, go, true);
}

const wsContainer = document.querySelector('#ws-container');
wsContainer.addEventListener('htmx:wsAfterMessage', event => {
	audioSource = document.querySelector('#audio-source');
	if (currentSource != audioSource.src) {
		currentSource = audioSource.src;
		player.load();
		if (playAfterSwitch) {
			playAfterSwitch = false;
			startPlayback();
		}
	}
});

// Media Session API: surfaces playback metadata and controls to the OS
// (Android lock screen, Bluetooth car head units, headset buttons, etc.).
const mediaArtwork = [
	{ src: new URL('/static/icon-192.png', location.href).href, sizes: '192x192', type: 'image/png' },
	{ src: new URL('/static/icon-512.png', location.href).href, sizes: '512x512', type: 'image/png' },
];

function updateMediaSessionMetadata() {
	if (!('mediaSession' in navigator)) return;

	// Re-query each call: htmx hx-swap-oob *replaces* the elements with
	// matching IDs, so any cached references would point at detached nodes
	// and return empty text after the first WS update.
	const stationNameEl = document.querySelector('#station-name');
	const stationTitleEl = document.querySelector('#station-title');
	if (!stationNameEl || !stationTitleEl) return;

	// #station-name is rendered as "[Name]" inside an <a>; strip the brackets.
	const rawName = (stationNameEl.innerText || '').trim();
	const name = rawName.replace(/^\[/, '').replace(/\]$/, '');
	const rawTitle = (stationTitleEl.innerText || '').trim();

	const title = rawTitle || name || 'Internet Radio';
	const artist = name || 'Internet Radio';

	if (!navigator.mediaSession.metadata) {
		navigator.mediaSession.metadata = new MediaMetadata({
			title,
			artist,
			album: 'Internet Radio',
			artwork: mediaArtwork,
		});
		return;
	}

	const md = navigator.mediaSession.metadata;
	if (md.title !== title) md.title = title;
	if (md.artist !== artist) md.artist = artist;
}

if ('mediaSession' in navigator) {
	navigator.mediaSession.setActionHandler('play', () => {
		document.querySelector('#play-pause-button').click();
	});
	navigator.mediaSession.setActionHandler('pause', () => {
		document.querySelector('#play-pause-button').click();
	});
	navigator.mediaSession.setActionHandler('previoustrack', () => {
		document.querySelector('#button-ws-prev').click();
	});
	navigator.mediaSession.setActionHandler('nexttrack', () => {
		document.querySelector('#button-ws-next').click();
	});

	player.addEventListener('play', () => {
		navigator.mediaSession.playbackState = 'playing';
		updateMediaSessionMetadata();
	});
	player.addEventListener('pause', () => {
		navigator.mediaSession.playbackState = 'paused';
	});

	// Refresh metadata whenever the server pushes new station info.
	wsContainer.addEventListener('htmx:wsAfterMessage', updateMediaSessionMetadata);
	updateMediaSessionMetadata();
}

// Volume slider
const volume = document.querySelector('#volume-slider');
// Keep volume across station changes (issue #3) and page reloads.
try {
	const saved = localStorage.getItem('internet-radio-proxifier-volume');
	if (saved !== null) volume.value = saved;
} catch (e) {}
const volumeValue = document.querySelector('#volume-value');
function applyVolume() {
	player.volume = Number(volume.value);
	volumeValue.textContent = Math.round(volume.value * 100) + '%';
}
volume.addEventListener('input', () => {
	applyVolume();
	try { localStorage.setItem('internet-radio-proxifier-volume', volume.value); } catch (e) {}
});
for (const ev of ['loadstart', 'loadedmetadata', 'playing']) {
	player.addEventListener(ev, applyVolume);
}
applyVolume();

// Pause/Play button
const playPauseButton = document.querySelector('#play-pause-button');
let isPlaying = false;

player.addEventListener('play', () => {
	playPauseButton.textContent = 'pause';
	isPlaying = true;
});

player.addEventListener('pause', () => {
	playPauseButton.textContent = 'play';
	isPlaying = false;
});

playPauseButton.addEventListener('click', () => {
	if (isPlaying) {
		player.pause();
		playPauseButton.textContent = 'Play';
	} else {
		player.play();
		playPauseButton.textContent = 'Pause';
	}
});

// Auto-skip: if the current station errors, ends, or stops delivering audio,
// move on to the next station. Only active once playback has started at least
// once (so a browser autoplay block doesn't cause skipping) and while the user
// hasn't paused. Gives up after one full round of failing stations.
const SKIP_STALL_MS = 12000;
const SKIP_MAX_IN_A_ROW = 12;
let autoSkipActive = false;
let lastTime = 0;
let lastProgressAt = Date.now();
let skipsInARow = 0;
let goodSince = 0;

player.addEventListener('playing', () => { autoSkipActive = true; });
playPauseButton.addEventListener('click', () => {
	// The click handler above has already toggled play/pause.
	autoSkipActive = !player.paused;
	if (!autoSkipActive) skipsInARow = 0;
	lastProgressAt = Date.now();
});
for (const id of ['#button-ws-next', '#button-ws-prev']) {
	document.querySelector(id).addEventListener('click', e => {
		if (e.isTrusted) skipsInARow = 0; // manual switch resets the give-up counter
		lastProgressAt = Date.now();
		lastTime = 0;
	});
}

function skipStation(reason) {
	if (!autoSkipActive) return;
	if (skipsInARow >= SKIP_MAX_IN_A_ROW) {
		status.innerText = 'No working station found';
		autoSkipActive = false;
		return;
	}
	skipsInARow++;
	console.log('auto-skip:', reason, currentSource);
	status.innerText = 'Station unavailable, skipping…';
	lastProgressAt = Date.now();
	lastTime = 0;
	document.querySelector('#button-ws-next').click();
}

// <source> errors don't bubble, so listen in the capture phase.
player.addEventListener('error', () => skipStation('error'), true);
player.addEventListener('ended', () => skipStation('ended'));

setInterval(() => {
	if (!autoSkipActive) return;
	const now = Date.now();
	if (player.currentTime !== lastTime) {
		if (player.currentTime > lastTime) {
			if (!goodSince) goodSince = now;
			if (now - goodSince > 5000 && skipsInARow) {
				skipsInARow = 0;
				status.innerText = 'Connected';
			}
		}
		lastTime = player.currentTime;
		lastProgressAt = now;
		return;
	}
	goodSince = 0;
	if (now - lastProgressAt > SKIP_STALL_MS) skipStation('stalled');
}, 1000);

// Station picker: list all stations and jump straight to one.
const stationDialog = document.querySelector('#station-dialog');
const stationPicker = document.querySelector('#station-picker');
const stationFilter = document.querySelector('#station-filter');

function currentStationName() {
	return (document.querySelector('#station-name')?.innerText || '').trim().replace(/^\[(.*)\]$/, '$1');
}

function applyStationFilter() {
	const q = stationFilter.value.trim().toLowerCase();
	for (const li of stationPicker.children) {
		li.hidden = q !== '' && !li.textContent.toLowerCase().includes(q);
	}
}

async function openStationPicker() {
	let stations = [];
	try {
		const res = await fetch('/api/stations');
		stations = await res.json();
	} catch (e) {
		status.innerText = 'Could not load station list';
		return;
	}
	const cur = currentStationName();
	stationPicker.replaceChildren(...stations.map(({name, proxy}, i) => {
		const li = document.createElement('li');
		const b = document.createElement('button');
		b.type = 'button';
		b.className = 'station-pick';
		b.dataset.index = i;
		const label = document.createElement('span');
		label.className = 'pick-name';
		label.textContent = name;
		b.appendChild(label);
		if (proxy) {
			const tag = document.createElement('span');
			tag.className = 'pick-proxy';
			tag.textContent = 'proxy';
			tag.title = 'Plays through the proxy';
			b.appendChild(tag);
		}
		if (name === cur) {
			b.setAttribute('aria-current', 'true');
		}
		li.appendChild(b);
		return li;
	}));
	stationFilter.value = '';
	stationDialog.showModal();
	const active = stationPicker.querySelector('[aria-current]');
	if (active) {
		active.scrollIntoView({block: 'center'});
		active.focus();
	}
}

document.querySelector('#button-stations').addEventListener('click', openStationPicker);
document.querySelector('.station-dialog-close').addEventListener('click', () => stationDialog.close());
stationFilter.addEventListener('input', applyStationFilter);
stationFilter.addEventListener('keydown', e => {
	if (e.key === 'Enter') {
		stationPicker.querySelector('li:not([hidden]) button')?.click();
	}
});
// Clicking the backdrop closes the dialog.
stationDialog.addEventListener('click', e => {
	if (e.target === stationDialog) stationDialog.close();
});

// Switch to station `index` and play it, even if the user had paused; it
// starts once the server has sent the new source.
function selectStation(index) {
	if (!socket) return;
	socket.send(JSON.stringify({action: 'select', index}), elt);
	skipsInARow = 0;
	lastProgressAt = Date.now();
	lastTime = 0;
	autoSkipActive = true;
	playAfterSwitch = true;
}

stationPicker.addEventListener('click', e => {
	const b = e.target.closest('.station-pick');
	if (!b) return;
	selectStation(Number(b.dataset.index));
	stationDialog.close();
});

// Random station, never the one that is playing now.
document.querySelector('#button-random').addEventListener('click', async () => {
	let count = 0;
	try {
		count = (await (await fetch('/api/stations')).json()).length;
	} catch (e) {
		status.innerText = 'Could not load station list';
		return;
	}
	if (count < 2) return;
	const cur = Number(document.querySelector('#audio-source')?.dataset.index ?? -1);
	let next = Math.floor(Math.random() * (count - 1));
	if (next >= cur && cur >= 0) next++;
	selectStation(next);
});

// Remember the last station in this browser. Stored by name, so reordering
// the list on the settings page doesn't change which station comes back.
const LAST_STATION_KEY = 'internet-radio-proxifier-last-station';
let wantedStation = null;
try { wantedStation = localStorage.getItem(LAST_STATION_KEY); } catch (e) {}
let firstConnect = true;

// Don't start the default station first if another one will be restored.
if (wantedStation) {
	player.pause();
} else if (AUTOPLAY) {
	startPlayback(); // play the default station as soon as the page opens
}

let stationNames = [];
async function loadStationNames() {
	try {
		stationNames = (await (await fetch('/api/stations')).json()).map(s => s.name);
	} catch (e) {}
	return stationNames;
}

function saveStation(name) {
	if (!name) return;
	wantedStation = name;
	try { localStorage.setItem(LAST_STATION_KEY, name); } catch (e) {}
}

// The server sends the station index with every new source (next, previous,
// picker, auto-skip), so the choice is saved right away.
let lastSavedIndex = null;
wsContainer.addEventListener('htmx:wsAfterMessage', async () => {
	if (firstConnect) return; // still restoring; ignore the default station
	const idx = document.querySelector('#audio-source')?.dataset.index;
	if (idx === undefined || idx === lastSavedIndex) return;
	lastSavedIndex = idx;
	await loadStationNames(); // fresh: the list may have been reordered
	saveStation(stationNames[idx]);
});

// On every (re)connect the server starts this session at station 0, so ask
// for the wanted one again. On reconnect the same source is kept playing.
document.body.addEventListener('htmx:wsOpen', async () => {
	const restoring = firstConnect;
	let index = -1;
	if (wantedStation) {
		index = (await loadStationNames()).indexOf(wantedStation);
	}
	if (index > 0) {
		if (restoring) playAfterSwitch = AUTOPLAY;
		socket.send(JSON.stringify({action: 'select', index}), elt);
	} else if (restoring && wantedStation && AUTOPLAY) {
		// Saved station is station 0 or no longer exists: play the default.
		startPlayback();
	}
	firstConnect = false;
});
