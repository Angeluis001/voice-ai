
    document.addEventListener('click', async (event) => {
      const btn = event.target.closest('.copy-content-btn');
      if (!btn) return;
      const body = btn.getAttribute('data-copy-body') || '';
      try {
        await navigator.clipboard.writeText(body);
        const prev = btn.textContent;
        btn.textContent = 'Copied!';
        setTimeout(() => { btn.textContent = prev || 'Copy'; }, 1400);
      } catch (e) {
        btn.textContent = 'Failed';
        setTimeout(() => { btn.textContent = 'Copy'; }, 1400);
      }
    });
    