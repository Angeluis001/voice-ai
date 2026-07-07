const Srf = require('drachtio-srf');

const srf = new Srf();

const host = process.env.DRACHTIO_HOST || 'drachtio-server';
const port = Number(process.env.DRACHTIO_PORT || 9022);
const secret = process.env.DRACHTIO_SECRET || '';
const fallbackTargetUri = process.env.B2B_TARGET_URI || 'sip:9196@freeswitch:5080';
const bridgeHost = process.env.FREESWITCH_BRIDGE_HOST || 'freeswitch';
const bridgePort = Number(process.env.FREESWITCH_BRIDGE_PORT || 5062);

function buildTargetUri(req) {
  const requestUriUser = req.uri?.user;
  const toUser = req.getParsedHeader('To')?.uri?.user;
  const calledUser = requestUriUser || toUser;

  if (!calledUser || calledUser === '9196') {
    return fallbackTargetUri;
  }

  return `sip:${calledUser}@${bridgeHost}:${bridgePort}`;
}

srf
  .on('connect', (_err, hp) => {
    console.log(`drachtio-app connected to ${hp}`);
  })
  .on('error', (err) => {
    console.error('drachtio-app connection error:', err);
  });

srf.connect({host, port, secret});

srf.invite(async(req, res) => {
  const callId = req.get('Call-ID') || 'unknown-call-id';
  const from = req.getParsedHeader('From')?.uri || 'unknown-from';
  const to = req.getParsedHeader('To')?.uri || 'unknown-to';
  const targetUri = buildTargetUri(req);

  console.log(`incoming INVITE call-id=${callId} from=${from} to=${to}`);
  console.log(`bridging call-id=${callId} to ${targetUri}`);

  try {
    const {uas, uac} = await srf.createB2BUA(req, res, targetUri, {
      headers: {
        'X-Voice-AI-Call-ID': callId,
        'X-Voice-AI-Bridge': targetUri === fallbackTargetUri ? 'freeswitch-echo-mvp' : 'freeswitch-did-mvp'
      }
    });

    console.log(`bridge established call-id=${callId}`);

    uas.on('destroy', () => {
      console.log(`uas leg ended call-id=${callId}`);
      if (!uac.destroyed) uac.destroy().catch(() => undefined);
    });

    uac.on('destroy', () => {
      console.log(`uac leg ended call-id=${callId}`);
      if (!uas.destroyed) uas.destroy().catch(() => undefined);
    });
  } catch (err) {
    console.error(`bridge failed call-id=${callId}:`, err);

    if (!res.finalResponseSent) {
      res.send(err.status || 500, err.reason || 'bridge failed');
    }
  }
});
