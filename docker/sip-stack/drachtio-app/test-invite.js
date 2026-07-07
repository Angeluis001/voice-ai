const Srf = require('drachtio-srf');

const srf = new Srf();

const host = process.env.DRACHTIO_HOST || 'drachtio-server';
const port = Number(process.env.DRACHTIO_PORT || 9022);
const secret = process.env.DRACHTIO_SECRET || '';
const targetUri = process.env.TEST_INVITE_URI || 'sip:9196@kamailio:5060';

srf.on('connect', async() => {
  console.log(`test-invite connected to ${host}:${port}`);

  try {
    const dialog = await srf.createUAC(targetUri, {
      headers: {
        From: '<sip:testcaller@voice-ai.local>'
      }
    });

    console.log(`test-invite established dialog to ${targetUri}`);

    setTimeout(async() => {
      try {
        await dialog.destroy();
        console.log('test-invite ended dialog cleanly');
      } catch (err) {
        console.error('test-invite failed to end dialog:', err);
      } finally {
        process.exit(0);
      }
    }, 3000);
  } catch (err) {
    console.error('test-invite failed:', err);
    process.exit(1);
  }
});

srf.on('error', (err) => {
  console.error('test-invite connection error:', err);
});

srf.connect({host, port, secret});
