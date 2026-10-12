/**
 * Usage: node scripts/svelte/compare-browser.mjs OFFICIAL_APP NATIVE_APP NEW_OUTPUT [--pages N] [--allow ORIGIN]...
 *
 * Compare two adapter-node builds of one SvelteKit app in headless Chromium.
 * Each APP directory holds build/ (and node_modules when the server needs
 * runtime dependencies). Pages are discovered by a breadth-first crawl of
 * same-origin links from "/" on the official server (at most N pages, default
 * 25), then loaded on both servers. Per page: status, final URL, title, body
 * text, page errors, failed same-origin requests (cancelled ones excepted) and
 * a screenshot. One link is
 * followed per page to check that navigation is client-side (a hydrated app).
 * Static files are compared over HTTP (Accept-Encoding: br, gzip): files at
 * the same build/client path by status and headers, and hashed
 * `_app/immutable` files summarized per extension (content type, encoding,
 * cache control), since their names differ between builds.
 * Browser requests to other origins are blocked unless --allow names them;
 * server-side requests are not controlled. Env: PLAYWRIGHT_PACKAGE (directory
 * of the playwright package), PLAYWRIGHT_CHROMIUM (browser executable).
 */
import { spawn } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { mkdir, readdir, writeFile } from 'node:fs/promises';
import { extname, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
if (args.length < 3 || args.includes('--help')) {
	console.log('Usage: node scripts/svelte/compare-browser.mjs OFFICIAL_APP NATIVE_APP NEW_OUTPUT [--pages N] [--allow ORIGIN]...');
	process.exit(args.includes('--help') ? 0 : 2);
}
const [official, native, out] = args.slice(0, 3).map((p) => resolve(p));
let maxPages = 25;
const allowed = [];
for (let i = 3; i < args.length; i += 2) {
	if (args[i] === '--pages') maxPages = Number(args[i + 1]);
	else if (args[i] === '--allow') allowed.push(args[i + 1]);
	else throw new Error(`Unknown argument ${args[i]}`);
}
await mkdir(out);
const playwright = process.env.PLAYWRIGHT_PACKAGE ?? resolve('target/svelte-project-build-official-1/app/node_modules/playwright');
const { chromium } = await import(pathToFileURL(resolve(playwright, 'index.mjs')));
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM, headless: true, args: ['--no-sandbox'] });

async function startServer(mode, app, port) {
	const origin = `http://127.0.0.1:${port}`;
	const log = createWriteStream(`${out}/${mode}-server.log`);
	const server = spawn(process.execPath, ['build/index.js'], {
		cwd: app,
		env: { PATH: process.env.PATH, NODE_ENV: 'production', HOST: '127.0.0.1', PORT: String(port), ORIGIN: origin }
	});
	server.stdout.pipe(log);
	server.stderr.pipe(log);
	for (let n = 0; n < 150; n++) {
		try {
			await fetch(origin + '/', { redirect: 'manual' });
			return { origin, server, log };
		} catch {
			await new Promise((r) => setTimeout(r, 100));
		}
	}
	server.kill('SIGKILL');
	throw new Error(`${mode} server did not start`);
}

async function stopServer({ server, log }) {
	const exited = new Promise((r) => server.on('exit', r));
	server.kill();
	const force = setTimeout(() => server.kill('SIGKILL'), 3000);
	await exited;
	clearTimeout(force);
	log.end();
}

async function newContext(origin) {
	const context = await browser.newContext();
	await context.route('**/*', (route) => {
		const url = route.request().url();
		return url.startsWith(origin) || allowed.some((a) => url.startsWith(a)) ? route.continue() : route.abort();
	});
	return context;
}

async function visit(context, origin, mode, path) {
	const page = await context.newPage();
	const errors = [], failed = [], blocked = [];
	page.on('pageerror', (e) => errors.push(e.message));
	page.on('requestfailed', (r) => {
		const url = r.url();
		// A request cancelled by the next navigation is not a failure.
		if (r.failure()?.errorText === 'net::ERR_ABORTED') return;
		if (url.startsWith(origin)) failed.push(url.slice(origin.length));
		else blocked.push(new URL(url).origin);
	});
	const record = { path, errors, failed };
	try {
		const response = await page.goto(origin + path, { waitUntil: 'networkidle', timeout: 30000 });
		record.status = response?.status() ?? null;
		record.url = page.url().replace(origin, '');
		record.title = await page.title();
		record.text = (await page.locator('body').innerText()).slice(0, 20000);
		record.links = [...new Set(await page.$$eval('a[href]', (as) => as.map((a) => a.href)))]
			.filter((href) => href.startsWith(origin))
			.map((href) => { const u = new URL(href); return u.pathname + u.search; })
			.sort();
		await page.screenshot({ path: `${out}/${mode}-${path.replaceAll(/[^a-z0-9]+/gi, '_') || 'root'}.png`, fullPage: true });
		// Client-side navigation: a marker set on window survives only if the
		// router handles the click without a document load.
		const target = record.links.find((l) => l !== path && !l.includes('logout'));
		if (target) {
			await page.evaluate(() => { window.__nativeCompareMarker = 1; });
			const link = page.locator(`a[href="${target}"], a[href="${origin}${target}"]`).first();
			if (await link.count() && await link.isVisible()) {
				await Promise.all([page.waitForURL((u) => u.pathname + u.search === target, { timeout: 10000 }).catch(() => {}), link.click()]);
				await page.waitForLoadState('networkidle').catch(() => {});
				record.clientNavigation = { to: target, reachedUrl: page.url().replace(origin, ''), clientSide: await page.evaluate(() => window.__nativeCompareMarker === 1) };
			}
		}
	} catch (error) {
		record.error = String(error.message);
	}
	record.blockedOrigins = [...new Set(blocked)].sort();
	await page.close();
	return record;
}

/** Relative file paths under DIR, without precompressed variants. */
async function clientFiles(dir) {
	const files = [];
	async function walk(relative) {
		for (const entry of await readdir(resolve(dir, relative), { withFileTypes: true }).catch(() => [])) {
			const name = relative ? `${relative}/${entry.name}` : entry.name;
			if (entry.isDirectory()) await walk(name);
			else if (!/\.(br|gz)$/.test(name)) files.push(name);
		}
	}
	await walk('');
	return files.sort();
}

const HEADERS = ['content-type', 'content-encoding', 'cache-control', 'vary'];
async function staticHeaders(origin, app) {
	const files = await clientFiles(resolve(app, 'build/client'));
	const plain = {}, immutable = {};
	for (const file of files) {
		const response = await fetch(`${origin}/${file}`, { headers: { 'accept-encoding': 'br, gzip' } });
		await response.arrayBuffer();
		const headers = Object.fromEntries(HEADERS.map((name) => [name, response.headers.get(name)]));
		const record = { status: response.status, ...headers };
		if (file.startsWith('_app/immutable/')) {
			const key = extname(file);
			const summary = JSON.stringify(record);
			(immutable[key] ??= new Set()).add(summary);
		} else plain[file] = record;
	}
	return { plain, immutable: Object.fromEntries(Object.entries(immutable).map(([k, v]) => [k, [...v].sort()])) };
}

const reports = {};
const headers = {};
try {
	// Discover pages on the official server.
	const officialServer = await startServer('official', official, 49181);
	let pages = [];
	try {
		const context = await newContext(officialServer.origin);
		const queue = ['/'], seen = new Set(queue), records = [];
		while (queue.length && records.length < maxPages) {
			const record = await visit(context, officialServer.origin, 'official', queue.shift());
			records.push(record);
			for (const link of record.links ?? []) if (!seen.has(link)) { seen.add(link); queue.push(link); }
		}
		await context.close();
		reports.official = records;
		headers.official = await staticHeaders(officialServer.origin, official);
		pages = records.map((r) => r.path);
	} finally {
		await stopServer(officialServer);
	}
	const nativeServer = await startServer('native', native, 49182);
	try {
		const context = await newContext(nativeServer.origin);
		reports.native = [];
		for (const path of pages) reports.native.push(await visit(context, nativeServer.origin, 'native', path));
		await context.close();
		headers.native = await staticHeaders(nativeServer.origin, native);
	} finally {
		await stopServer(nativeServer);
	}
} finally {
	await browser.close();
}

const comparisons = reports.official.map((a, i) => {
	const b = reports.native[i];
	const differences = [];
	for (const key of ['status', 'url', 'title', 'text', 'error'])
		if (a[key] !== b[key]) differences.push(key);
	if (JSON.stringify(a.errors) !== JSON.stringify(b.errors)) differences.push('pageErrors');
	if (JSON.stringify(a.failed) !== JSON.stringify(b.failed)) differences.push('failedRequests');
	if (a.clientNavigation?.clientSide !== b.clientNavigation?.clientSide) differences.push('clientNavigation');
	return {
		path: a.path, matched: differences.length === 0, differences,
		officialStatus: a.status, nativeStatus: b.status,
		nativeErrors: b.errors, nativeFailed: b.failed,
		clientSide: { official: a.clientNavigation?.clientSide ?? null, native: b.clientNavigation?.clientSide ?? null }
	};
});
// Static files: same-path files must agree; immutable summaries per extension.
const staticDifferences = [];
for (const [file, record] of Object.entries(headers.official.plain)) {
	const other = headers.native.plain[file];
	if (JSON.stringify(record) !== JSON.stringify(other)) staticDifferences.push({ file, official: record, native: other ?? null });
}
for (const file of Object.keys(headers.native.plain))
	if (!(file in headers.official.plain)) staticDifferences.push({ file, official: null, native: headers.native.plain[file] });
for (const key of new Set([...Object.keys(headers.official.immutable), ...Object.keys(headers.native.immutable)])) {
	const a = headers.official.immutable[key], b = headers.native.immutable[key];
	if (JSON.stringify(a) !== JSON.stringify(b)) staticDifferences.push({ immutableExtension: key, official: a ?? null, native: b ?? null });
}
const result = { pages: comparisons.length, matched: comparisons.filter((c) => c.matched).length, staticMatched: staticDifferences.length === 0, staticDifferences, comparisons, reports, headers };
await writeFile(`${out}/report.json`, JSON.stringify(result, null, 2) + '\n');
console.log(JSON.stringify({ pages: result.pages, matched: result.matched, staticMatched: result.staticMatched, staticDifferences, comparisons: comparisons.map(({ path, matched, differences, officialStatus, nativeStatus, clientSide }) => ({ path, matched, differences, officialStatus, nativeStatus, clientSide })) }, null, 1));
process.exitCode = result.matched === result.pages && result.staticMatched ? 0 : 1;
