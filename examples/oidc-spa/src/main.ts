// A minimal SPA that signs in through rackend's OIDC provider with the
// standard `oidc-client-ts` library: authorization code + PKCE, the
// `default` client, then an API call with the access token.
import { UserManager, WebStorageStateStore, type User } from 'oidc-client-ts';

const params = new URLSearchParams(location.search);
const authority = params.get('authority') ?? localStorage.getItem('authority') ?? 'http://localhost:8090';
localStorage.setItem('authority', authority);
const here = location.origin + location.pathname;

const manager = new UserManager({
	authority,
	client_id: params.get('client_id') ?? 'default',
	redirect_uri: here,
	post_logout_redirect_uri: here,
	response_type: 'code',
	scope: 'openid profile email offline_access',
	userStore: new WebStorageStateStore({ store: sessionStorage })
});

const $ = (id: string) => document.getElementById(id) as HTMLElement;
$('authority').textContent = authority;

function show(user: User | null, extra?: unknown) {
	const signedIn = !!user && !user.expired;
	$('login').hidden = signedIn;
	$('api').hidden = !signedIn;
	$('logout').hidden = !signedIn;
	$('status').textContent = signedIn
		? JSON.stringify({ profile: user!.profile, scope: user!.scope, expires_in: user!.expires_in, ...(extra ? { api: extra } : {}) }, null, 2)
		: 'Signed out.';
	document.body.dataset.state = signedIn ? 'signed-in' : 'signed-out';
}

async function callApi() {
	const user = await manager.getUser();
	if (!user) return;
	const res = await fetch(`${authority}/api/collections/users/records/${user.profile.sub}`, {
		headers: { Authorization: `Bearer ${user.access_token}` }
	});
	const body = await res.json();
	show(user, { status: res.status, email: body.email, id: body.id });
	document.body.dataset.api = String(res.status);
}

$('login').addEventListener('click', () => manager.signinRedirect());
$('logout').addEventListener('click', () => manager.signoutRedirect());
$('api').addEventListener('click', () => void callApi());

if (params.has('code') && params.has('state')) {
	manager
		.signinRedirectCallback()
		.then((user) => {
			history.replaceState(null, '', here);
			show(user);
		})
		.catch((e: Error) => ($('status').textContent = `Sign-in failed: ${e.message}`));
} else if (params.has('error')) {
	$('status').textContent = `Sign-in failed: ${params.get('error')} ${params.get('error_description') ?? ''}`;
} else {
	manager.getUser().then((u) => show(u));
}
