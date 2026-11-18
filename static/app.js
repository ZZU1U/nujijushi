/* nujijushi admin UI — progressive enhancement only.
 *
 * Nothing in this file is required to use the admin: with JS disabled (or this
 * request failing) every page still renders, submits, filters and paginates,
 * because every feature here only adds state on top of server-rendered HTML.
 * Loaded with `defer`, so the DOM is parsed before it runs.
 */
(function () {
  'use strict';

  const POLL_MS = 3000; // import job table
  const QUIET_POLLS_TO_STOP = 2; // consecutive polls with nothing pending/running
  const MAX_POLL_ERRORS = 3; // give up on a server that stops answering
  const BUSY_MAX_MS = 15000; // never leave the result grid dimmed forever
  const LIVE_STATES = ['pending', 'running'];

  const path = location.pathname.replace(/\/+$/, '') || '/';
  const all = (selector, root) => Array.from((root || document).querySelectorAll(selector));
  const text = (node) => (node.textContent || '').replace(/\s+/g, ' ').trim();

  /* ------------------------------------------------------------------ forms
   * A submitted POST form is locked until the page goes away, so a double
   * click cannot import twice or queue the same post twice.
   */
  function restoreForms() {
    all('[data-submitting]').forEach((el) => {
      delete el.dataset.submitting;
      el.removeAttribute('aria-disabled');
    });
    all('form[aria-busy]').forEach((form) => form.removeAttribute('aria-busy'));
  }

  function guardDoubleSubmit() {
    document.addEventListener('submit', (event) => {
      const form = event.target;
      if (!(form instanceof HTMLFormElement)) return;
      if ((form.getAttribute('method') || 'get').toLowerCase() !== 'post') return;
      if (form.dataset.submitting === '1') {
        event.preventDefault(); // a submit is already in flight
        return;
      }
      const button =
        event.submitter || form.querySelector('button[type="submit"], button:not([type])');
      form.dataset.submitting = '1';
      form.setAttribute('aria-busy', 'true');
      if (button) {
        // NOT `disabled`: that would drop the button's name/value from the
        // payload, and the bulk actions on /posts are `name="action"` buttons.
        button.dataset.submitting = '1';
        button.setAttribute('aria-disabled', 'true');
      }
      // another handler may cancel the submit (the delete confirm does); don't
      // leave the form spinning if it was
      setTimeout(() => {
        if (!event.defaultPrevented) return;
        form.removeAttribute('aria-busy');
        delete form.dataset.submitting;
        if (button) {
          delete button.dataset.submitting;
          button.removeAttribute('aria-disabled');
        }
      }, 0);
    });
    window.addEventListener('pageshow', restoreForms); // back/forward cache
  }

  /* ---------------------------------------------------------- /posts filters
   * A filter change starts a full navigation; dim the results so the click
   * lands somewhere instead of on a frozen page.
   */
  function filterBusyState() {
    if (path !== '/posts') return;
    const filters = document.querySelector('form.filters');
    const results = document.querySelector('#bulk .grid') || document.querySelector('#bulk');
    if (!filters || !results) return;

    let timer = null;
    const settle = () => {
      clearTimeout(timer);
      results.classList.remove('is-busy');
      results.removeAttribute('aria-busy');
    };
    const busy = () => {
      results.classList.add('is-busy');
      results.setAttribute('aria-busy', 'true');
      clearTimeout(timer);
      timer = setTimeout(settle, BUSY_MAX_MS);
    };

    filters.addEventListener('change', busy);
    filters.addEventListener('submit', busy);
    window.addEventListener('pageshow', settle);
  }

  /* ---------------------------------------------------------------- keyboard
   * `/` jumps to search, the way every archive tool does it.
   */
  function slashToSearch() {
    if (path !== '/posts') return;
    const search = document.querySelector('form.filters input[name="search"]');
    if (!search) return;
    const isField = (el) => {
      if (!el || !el.tagName) return false;
      const tag = el.tagName.toLowerCase();
      return tag === 'input' || tag === 'textarea' || tag === 'select' || el.isContentEditable === true;
    };
    document.addEventListener('keydown', (event) => {
      if (event.isComposing || event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key === 'Escape' && event.target === search) {
        search.blur();
        return;
      }
      if (event.key !== '/' || isField(event.target)) return;
      event.preventDefault();
      search.focus();
      search.select();
    });
  }

  /* -------------------------------------------------------- /import job table
   * The jobs panel re-renders itself while a job is pending or running, then
   * stops on its own so an idle tab is not polling forever.
   */
  function liveImportJobs() {
    if (path !== '/import') return;
    if (typeof window.fetch !== 'function' || typeof window.DOMParser !== 'function') return;

    const findPanel = (root) => {
      const table = (root || document).querySelector('.panel table.compact');
      return table ? table.closest('.panel') : null;
    };

    let panel = findPanel(document);
    if (!panel) return; // no jobs panel on this render: nothing to watch

    let timer = null;
    let quietPolls = 0;
    let errors = 0;
    let inFlight = false;
    let stopped = false;

    panel.dataset.live = 'import-jobs';

    const hasLiveBadge = (root) =>
      all('.badge', root).some((badge) => LIVE_STATES.indexOf(text(badge).toLowerCase()) !== -1);

    // state before a swap, keyed by job id, so only rows that really changed move
    const snapshot = (root) => {
      const before = new Map();
      all('table tr', root).forEach((row) => {
        const id = text(row.querySelector('td') || { textContent: '' });
        if (id) before.set(id, text(row));
      });
      return before;
    };

    const paintChanges = (root, before) => {
      let index = 0;
      all('table tr', root).forEach((row) => {
        const id = text(row.querySelector('td') || { textContent: '' });
        if (!id || before.get(id) === text(row)) return;
        row.dataset.changed = '1';
        row.style.setProperty('--i', String(index++));
      });
    };

    const stop = (state) => {
      stopped = true;
      clearTimeout(timer);
      inFlight = false;
      if (panel && panel.isConnected) panel.dataset.live = state;
    };

    const poll = () => {
      inFlight = true;
      return fetch(location.href, { credentials: 'same-origin', cache: 'no-store' })
        .then((response) => {
          if (!response.ok) throw new Error('HTTP ' + response.status);
          return response.text();
        })
        .then((html) => {
          const fresh = findPanel(new DOMParser().parseFromString(html, 'text/html'));
          if (!fresh) {
            quietPolls += 1; // the jobs table is gone, so nothing can be running
            return;
          }
          const before = snapshot(panel);
          fresh.dataset.live = 'import-jobs';
          fresh.classList.add('live-in');
          panel.replaceWith(fresh);
          panel = fresh;
          paintChanges(fresh, before);
          quietPolls = hasLiveBadge(fresh) ? 0 : quietPolls + 1;
        })
        .catch(() => {
          errors += 1;
          if (errors >= MAX_POLL_ERRORS) stop('stalled');
        })
        .then(() => {
          inFlight = false;
          if (!stopped && quietPolls >= QUIET_POLLS_TO_STOP) {
            stop('settled');
            const hint = panel && panel.querySelector('p.hint');
            if (hint && /^refresh to update/i.test(text(hint))) {
              hint.textContent = 'up to date · auto-refresh stopped, reload for new jobs';
            }
          }
        });
    };

    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(tick, POLL_MS);
    };

    function tick() {
      clearTimeout(timer);
      if (stopped || inFlight) return;
      if (document.hidden) return; // visibilitychange restarts the loop
      poll().then(() => {
        if (!stopped && !inFlight) schedule();
      });
    }

    document.addEventListener('visibilitychange', () => {
      if (document.hidden || stopped || inFlight) return;
      tick(); // back on the tab: refresh now instead of waiting out the interval
    });

    schedule();
  }

  guardDoubleSubmit();
  filterBusyState();
  slashToSearch();
  liveImportJobs();
})();
