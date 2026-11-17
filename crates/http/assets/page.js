// rackend hosted pages: passkey sign-in and small enhancements. The pages
// work without this script for passwords and email codes.
(function () {
  'use strict';

  // The "email me a code" form uses the address typed above it.
  var email = document.querySelector('input[name="email"][type="email"]');
  var mirrors = document.querySelectorAll('.mirror-email');
  if (email && mirrors.length) {
    var sync = function () {
      for (var i = 0; i < mirrors.length; i++) mirrors[i].value = email.value;
    };
    email.addEventListener('input', sync);
    sync();
  }

  var button = document.getElementById('passkey');
  if (!button || !window.PublicKeyCredential || !navigator.credentials) return;
  button.hidden = false;

  function b64urlToBuf(s) {
    s = s.replace(/-/g, '+').replace(/_/g, '/');
    while (s.length % 4) s += '=';
    var bin = atob(s), out = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out.buffer;
  }
  function bufToB64url(buf) {
    var bytes = new Uint8Array(buf), bin = '';
    for (var i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }
  function requestOptions(pk) {
    if (PublicKeyCredential.parseRequestOptionsFromJSON) return PublicKeyCredential.parseRequestOptionsFromJSON(pk);
    var o = Object.assign({}, pk, { challenge: b64urlToBuf(pk.challenge) });
    if (pk.allowCredentials) {
      o.allowCredentials = pk.allowCredentials.map(function (c) {
        return Object.assign({}, c, { id: b64urlToBuf(c.id) });
      });
    }
    return o;
  }
  function credentialJson(c) {
    if (c.toJSON) return c.toJSON();
    var r = c.response;
    return {
      id: c.id,
      rawId: bufToB64url(c.rawId),
      type: c.type,
      response: {
        authenticatorData: bufToB64url(r.authenticatorData),
        clientDataJSON: bufToB64url(r.clientDataJSON),
        signature: bufToB64url(r.signature),
        userHandle: r.userHandle ? bufToB64url(r.userHandle) : null
      },
      extensions: c.getClientExtensionResults ? c.getClientExtensionResults() : {}
    };
  }
  function fail(message) {
    var p = document.querySelector('.err[role="alert"]');
    if (!p) {
      p = document.createElement('p');
      p.className = 'err';
      p.setAttribute('role', 'alert');
      button.parentNode.insertBefore(p, button);
    }
    p.textContent = message;
    button.disabled = false;
  }
  function post(url, body) {
    return fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      credentials: 'same-origin',
      body: JSON.stringify(body)
    }).then(function (r) {
      return r.json().then(function (j) {
        if (!r.ok) throw new Error(j.detail || j.title || 'Request failed');
        return j;
      });
    });
  }

  button.addEventListener('click', function () {
    button.disabled = true;
    var collection = button.getAttribute('data-collection');
    var ar = button.getAttribute('data-ar');
    post('/api/collections/' + encodeURIComponent(collection) + '/passkeys/login/start', {})
      .then(function (start) {
        return navigator.credentials
          .get({ publicKey: requestOptions(start.options.publicKey) })
          .then(function (cred) {
            return post('/auth/login/passkey', { ar: ar, ceremony_id: start.ceremony_id, credential: credentialJson(cred) });
          });
      })
      .then(function (done) {
        window.location.assign(done.redirect);
      })
      .catch(function (e) {
        fail(e && e.name === 'NotAllowedError' ? 'The passkey sign-in was cancelled.' : 'Signing in with a passkey did not work.');
      });
  });
})();
