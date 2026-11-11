// Register the service worker so the app is installable as a PWA.
if ('serviceWorker' in navigator) {
	window.addEventListener('load', () => {
		navigator.serviceWorker.register('/sw.js').catch(err => {
			console.warn('Service worker registration failed:', err);
		});
	});
}

const status = document.querySelector('#status');

// The status line tells what the player is doing (connecting to a station,
// playing, buffering, paused, a station is unavailable). kind: '' (neutral),
// 'ok' or 'bad'.
function setStatus(text, kind = '') {
	status.innerText = text;
	status.dataset.status = kind;
}

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
// The websocket to this server only matters to the user when it is gone
// (station changes and titles stop working).
let serverLost = false;
document.body.addEventListener('htmx:wsClose', () => {
	serverLost = true;
	setStatus('No connection to the radio server, reconnecting…', 'bad');
});

document.body.addEventListener('htmx:wsOpen', event => {
	socket = event.detail.socketWrapper;
	elt = event.detail.elt;
	if (serverLost) {
		serverLost = false;
		showPlayerState();
	}
});

const player = document.querySelector('#audio-player');
// Settings page "Autoplay": start playing as soon as the page opens.
const AUTOPLAY = player.hasAttribute('data-autoplay');
// Settings page "Start with a random station" (only together with autoplay).
const RANDOM_START = player.hasAttribute('data-random-start');
// Settings page "Retry attempts": reconnects to a station that stopped
// playing before it is skipped.
const RETRIES = Number(player.dataset.retries ?? 3);

// Workaround to prevent player from playing when replaced
let audioSource = document.querySelector('#audio-source');
let currentSource = audioSource.src;
let playAfterSwitch = false;

// Start playback. Browsers block sound until the visitor has interacted with
// the page, so if this is refused, start on the first click, tap or key.
const GESTURE_EVENTS = ['pointerdown', 'keydown', 'touchstart'];
let waitingForGesture = false;
// Auto-skip (below) is on from here: a station that cannot be played is
// skipped even when nothing has played yet. Only the browser's autoplay
// block turns it off until the first click.
function startPlayback() {
	autoSkipActive = true;
	lastProgressAt = Date.now();
	lastTime = player.currentTime;
	player.play().catch(err => {
		if (err && err.name === 'NotAllowedError') {
			autoSkipActive = false;
			startOnFirstGesture();
		}
	});
}
function startOnFirstGesture() {
	if (waitingForGesture) return;
	waitingForGesture = true;
	document.body.classList.add('needs-gesture');
	setStatus('Click anywhere to start playing');
	const go = () => {
		for (const ev of GESTURE_EVENTS) document.removeEventListener(ev, go, true);
		waitingForGesture = false;
		document.body.classList.remove('needs-gesture');
		startPlayback();
	};
	for (const ev of GESTURE_EVENTS) document.addEventListener(ev, go, true);
}

const wsContainer = document.querySelector('#ws-container');
// A station name that arrives while connecting goes into the status line.
wsContainer.addEventListener('htmx:wsAfterMessage', () => {
	if (!stationStarted && !player.paused && !waitingForGesture && !gaveUp) connectingStatus();
});

wsContainer.addEventListener('htmx:wsAfterMessage', event => {
	audioSource = document.querySelector('#audio-source');
	if (currentSource != audioSource.src) {
		currentSource = audioSource.src;
		stationStarted = false;
		retriesLeft = RETRIES;
		clearTimeout(retryTimer);
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

// Status line from what the audio element does. failedStation: the station
// that was just skipped, mentioned until another one plays.
let failedStation = '';
// The current station has delivered audio since it was switched to; until
// then "waiting" means it is still connecting, not buffering.
let stationStarted = false;
// Set when auto-skip gives up; shown instead of "Paused" until playing again.
let gaveUp = '';
function showPlayerState() {
	if (waitingForGesture) return;
	if (gaveUp) setStatus(gaveUp, 'bad');
	else if (retrying) connectingStatus();
	else if (player.paused) setStatus('Paused');
	else if (player.readyState >= 3) setStatus('Playing', 'ok');
	else connectingStatus();
}
function connectingStatus() {
	if (retrying) return setStatus(retryText, 'bad');
	const name = currentStationName();
	const failed = failedStation ? `${failedStation} is unavailable. ` : '';
	setStatus(`${failed}Connecting to ${name || 'the station'}…`, failedStation ? 'bad' : '');
}
player.addEventListener('play', () => {
	gaveUp = '';
	if (!waitingForGesture) connectingStatus();
});
player.addEventListener('playing', () => {
	stationStarted = true;
	retrying = false;
	failedStation = '';
	setStatus('Playing', 'ok');
});
player.addEventListener('waiting', () => {
	if (player.paused || waitingForGesture) return;
	if (retrying) return;
	if (stationStarted) setStatus('Buffering…');
	else connectingStatus();
});
player.addEventListener('pause', showPlayerState);

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
// move on to the next station. Active while playback is wanted (see
// startPlayback) and the user hasn't paused. Gives up after one full round
// of failing stations.
const SKIP_STALL_MS = 12000;
let autoSkipActive = false;
let lastTime = 0;
let lastProgressAt = Date.now();
let skipsInARow = 0;
let goodSince = 0;

player.addEventListener('playing', () => { autoSkipActive = true; });
playPauseButton.addEventListener('click', () => {
	// The click handler above has already toggled play/pause.
	autoSkipActive = !player.paused;
	if (!autoSkipActive) {
		skipsInARow = 0;
		retrying = false;
		clearTimeout(retryTimer);
	}
	lastProgressAt = Date.now();
});
for (const id of ['#button-ws-next', '#button-ws-prev']) {
	document.querySelector(id).addEventListener('click', e => {
		if (e.isTrusted) {
			skipsInARow = 0; // manual switch resets the give-up counter
			failedStation = '';
		}
		// Next / previous (also from auto-skip) plays the new station.
		playAfterSwitch = true;
		lastProgressAt = Date.now();
		lastTime = 0;
	});
}

// A station that has played and then stops (the internet or the proxy
// reconnecting) is reconnected RETRIES times first, waiting a little longer
// each time (3 s, 6 s, 9 s, ...); one that never started is skipped right away.
const RETRY_DELAY_MS = 3000;
let retriesLeft = RETRIES;
let retrying = false;
let retryTimer = 0;
let retryText = '';

function retryStation(reason) {
	retriesLeft--;
	retrying = true;
	const attempt = RETRIES - retriesLeft;
	console.log(`retry ${attempt}/${RETRIES}:`, reason, currentSource);
	retryText = `${currentStationName() || 'The station'} stopped, reconnecting (${attempt}/${RETRIES})…`;
	setStatus(retryText, 'bad');
	const delay = RETRY_DELAY_MS * attempt;
	// No stall check until this attempt has had its time.
	lastProgressAt = Date.now() + delay;
	lastTime = 0;
	clearTimeout(retryTimer);
	retryTimer = setTimeout(() => {
		if (!autoSkipActive) return;
		lastProgressAt = Date.now();
		player.load();
		startPlayback();
	}, delay);
}

function skipStation(reason) {
	if (!autoSkipActive) return;
	if (stationStarted && retriesLeft > 0) {
		retryStation(reason);
		return;
	}
	retrying = false;
	if (skipsInARow >= Math.max(stationNames.length, 1)) {
		gaveUp = 'No station can be played right now. Is the internet connection working?';
		autoSkipActive = false;
		failedStation = '';
		player.pause();
		showPlayerState();
		return;
	}
	skipsInARow++;
	console.log('auto-skip:', reason, currentSource);
	failedStation = currentStationName();
	setStatus(`${failedStation || 'This station'} is unavailable, trying the next one…`, 'bad');
	lastProgressAt = Date.now();
	lastTime = 0;
	if (!socket) return; // the stall check tries again once reconnected
	playAfterSwitch = true;
	socket.send(JSON.stringify({action: 'next'}), elt);
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
			if (now - goodSince > 5000) {
				skipsInARow = 0;
				retriesLeft = RETRIES; // playing well again: a later drop gets all retries
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

// Don't start the default station first if another one will be restored
// or a random one picked.
if (wantedStation || RANDOM_START) {
	player.pause();
} else if (AUTOPLAY) {
	startPlayback(); // play the default station as soon as the page opens
}
if (!AUTOPLAY) setStatus('Press play to start');

let stationNames = [];
async function loadStationNames() {
	try {
		stationNames = (await (await fetch('/api/stations')).json()).map(s => s.name);
	} catch (e) {}
	return stationNames;
}
// The station count limits auto-skip to one round.
loadStationNames();

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
	if (restoring && RANDOM_START) {
		const names = await loadStationNames();
		index = Math.floor(Math.random() * names.length);
	} else if (wantedStation) {
		index = (await loadStationNames()).indexOf(wantedStation);
	}
	if (index > 0) {
		if (restoring) playAfterSwitch = AUTOPLAY;
		socket.send(JSON.stringify({action: 'select', index}), elt);
	} else if (restoring && (wantedStation || RANDOM_START) && AUTOPLAY) {
		// Station 0 was picked, or the saved one no longer exists: play the default.
		startPlayback();
	}
	firstConnect = false;
});
