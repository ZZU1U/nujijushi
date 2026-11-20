/* nujijushi admin UI — progressive enhancement only.
 *
 * Nothing in this file is required to use the admin: with JS disabled (or this
 * request failing) every page still renders, submits, filters and paginates,
 * because every feature here only adds state on top of server-rendered HTML.
 * Loaded with `defer`, so the DOM is parsed before it runs.
 */
(function () {
  'use strict';

  const POLL_MS = 2000; // /api/status while something is running
  const QUIET_POLLS_TO_STOP = 2; // consecutive polls with nothing live
  const MAX_POLL_ERRORS = 3; // give up on a server that stops answering
  const BUSY_MAX_MS = 15000; // never leave the result grid dimmed forever
  const TOAST_MS = 6000;
  const LIVE_STATES = ['pending', 'running'];

  const path = location.pathname.replace(/\/+$/, '') || '/';
  const all = (selector, root) => Array.from((root || document).querySelectorAll(selector));
  const text = (node) => (node.textContent || '').replace(/\s+/g, ' ').trim();
  const isLive = (state) => LIVE_STATES.indexOf(state) !== -1;

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
      const confirmation = (button && button.dataset.confirm) || form.dataset.confirm;
      if (confirmation && !window.confirm(confirmation)) {
        event.preventDefault();
        return;
      }
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

  function bulkPostSelection() {
    const form = document.querySelector('#bulk');
    const controls = form && form.querySelector('[data-post-selection]');
    if (!controls) return;
    const picks = all('[data-post-pick]', form);
    const selectAll = controls.querySelector('[data-select-posts]');
    const clear = controls.querySelector('[data-clear-posts]');
    const count = controls.querySelector('[data-post-selection-count]');
    const update = () => {
      const selected = picks.filter((pick) => pick.checked).length;
      setText(count, selected + ' selected');
      selectAll.disabled = selected === picks.length;
      clear.disabled = selected === 0;
    };
    const select = (checked) => {
      picks.forEach((pick) => { pick.checked = checked; });
      update();
    };
    selectAll.addEventListener('click', () => select(true));
    clear.addEventListener('click', () => select(false));
    form.addEventListener('change', update);
    window.addEventListener('pageshow', update);
    update();
    controls.hidden = false;
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

  /* ------------------------------------------------------------- live status
   * Every heavy action (publish, storage upload, import) is a job row now, so a
   * single JSON endpoint drives the whole UI: job progress bars, per-channel
   * queue depth, "publishing…" badges and a toast when a job settles. It stops
   * polling once nothing is live, so an idle tab stays quiet.
   */
  function toast(message, kind) {
    if (!message) return;
    let host = document.querySelector('.toasts');
    if (!host) {
      host = document.createElement('div');
      host.className = 'toasts';
      host.setAttribute('role', 'status');
      host.setAttribute('aria-live', 'polite');
      document.body.appendChild(host);
    }
    const item = document.createElement('div');
    item.className = 'toast' + (kind ? ' ' + kind : '');
    item.textContent = message;
    host.appendChild(item);
    setTimeout(() => {
      item.classList.add('out');
      setTimeout(() => item.remove(), 250);
    }, TOAST_MS);
  }

  function setText(node, value) {
    if (!node) return;
    const next = value === null || value === undefined ? '' : String(value);
    if (text(node) !== next.replace(/\s+/g, ' ').trim()) node.textContent = next;
  }

  function percent(job) {
    if (job.total > 0) return Math.max(0, Math.min(100, Math.round((job.progress * 100) / job.total)));
    return job.state === 'done' ? 100 : 0;
  }

  /// Repaint one job row; returns true when the job is still live.
  function paintJob(row, job) {
    const before = row.dataset.state;
    const wasLive = isLive(before);
    row.dataset.state = job.state;

    const badge = row.querySelector('[data-role="state"]');
    if (badge) {
      setText(badge, job.state);
      badge.classList.toggle('green', job.state === 'done');
      badge.classList.toggle('red', job.state === 'failed');
    }

    const bar = row.querySelector('.progress');
    if (bar) {
      const fill = bar.querySelector('span');
      if (fill) fill.style.width = percent(job) + '%';
      bar.setAttribute('aria-valuemax', String(job.total));
      bar.setAttribute('aria-valuenow', String(job.progress));
    }
    const counts = row.querySelector('[data-role="counts"]');
    setText(counts, job.progress + '/' + job.total);
    if (counts && row.hasAttribute('data-tag-review')) counts.hidden = job.total === 0;
    setText(row.querySelector('[data-role="message"]'), job.message);
    setText(row.querySelector('[data-role="age"]'), job.age);
    row.querySelectorAll('[data-watch-action]').forEach((button) => {
      button.disabled = isLive(job.state) || button.hasAttribute('data-watch-paused');
    });

    // a state change flashes the row once, so a poll result is visible in place
    if (before && before !== job.state) {
      delete row.dataset.changed;
      void row.offsetWidth; // restart the animation
      row.dataset.changed = '1';
    }

    if (wasLive && !isLive(job.state)) {
      setText(row.querySelector('[data-tag-refresh]'), job.state === 'done' ? 'Review suggestions' : 'See details');
      setText(row.querySelector('[data-merge-refresh]'), job.state === 'done' ? 'See updated gallery' : 'See details');
      const label = job.kind_label || job.kind;
      const detail = job.message ? ' — ' + job.message : '';
      toast(label + ' #' + job.id + ' ' + job.state + detail, job.state === 'done' ? 'ok' : 'fail');
    }
    return isLive(job.state);
  }

  /// Apply one `/api/status` document; returns true when anything is live.
  function applyStatus(status) {
    const jobs = Array.isArray(status && status.jobs) ? status.jobs : [];
    const byId = new Map(jobs.map((job) => [String(job.id), job]));
    const watchJobs = Array.isArray(status && status.watch_jobs) ? status.watch_jobs : [];
    watchJobs.forEach((job) => byId.set(String(job.id), job));
    const tagJob = status && status.tag_suggestion_job;
    if (tagJob) byId.set(String(tagJob.id), tagJob);
    const mergeJob = status && status.media_merge_job;
    if (mergeJob) byId.set(String(mergeJob.id), mergeJob);
    let live = false;

    all('[data-job]').forEach((row) => {
      const job = byId.get(row.dataset.job);
      if (job && paintJob(row, job)) live = true;
    });

    // dashboard: queue depth and an in-flight publish per channel
    const channels = Array.isArray(status && status.channels) ? status.channels : [];
    channels.forEach((channel) => {
      const row = document.querySelector('[data-channel="' + channel.id + '"]');
      if (!row) return;
      setText(row.querySelector('[data-role="queued"]'), channel.queued);
      const badge = row.querySelector('[data-role="publishing"]');
      if (badge) {
        badge.hidden = !channel.publishing;
        if (channel.publishing) live = true;
      }
    });

    // queue page: which item is being sent right now
    all('[data-queue]').forEach((row) => {
      const queued = jobs.find(
        (job) =>
          job.kind === 'publish_item' &&
          job.payload === row.dataset.queue &&
          isLive(job.state)
      );
      const badge = row.querySelector('[data-role="publishing"]');
      if (badge) badge.hidden = !queued;
      if (queued) live = true;
    });

    const indexing = document.querySelector('[data-duplicates-indexing]');
    if (indexing && status.duplicate_indexing) live = true;
    if (indexing && !status.duplicate_indexing) setText(indexing, 'Indexing complete. Reload for all matches.');
    return live;
  }

  function liveStatus() {
    if (typeof window.fetch !== 'function') return;
    if (!document.querySelector('[data-job], [data-channel], [data-queue], [data-duplicates-indexing]')) return;
    const tagReview = document.querySelector('[data-tag-review]');
    const merge = document.querySelector('[data-merge-post]');
    const params = new URLSearchParams();
    if (tagReview) params.set('tag_post', tagReview.dataset.tagReview);
    if (merge) params.set('merge_post', merge.dataset.mergePost);
    const statusUrl = '/api/status' + (params.toString() ? '?' + params.toString() : '');

    let timer = null;
    let quietPolls = 0;
    let errors = 0;
    let inFlight = false;
    let stopped = false;

    const stop = (state) => {
      stopped = true;
      clearTimeout(timer);
      inFlight = false;
      const hint = document.querySelector('[data-live-hint]');
      if (hint && state === 'settled') hint.textContent = 'up to date · reload for new rows';
    };

    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(tick, POLL_MS);
    };

    function tick() {
      clearTimeout(timer);
      if (stopped || inFlight || document.hidden) return;
      inFlight = true;
      fetch(statusUrl, {
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { accept: 'application/json' },
      })
        .then((response) => {
          if (!response.ok) throw new Error('HTTP ' + response.status);
          return response.json();
        })
        .then((status) => {
          errors = 0;
          quietPolls = applyStatus(status) ? 0 : quietPolls + 1;
        })
        .catch(() => {
          errors += 1;
          if (errors >= MAX_POLL_ERRORS) stop('stalled');
        })
        .then(() => {
          inFlight = false;
          if (stopped) return;
          if (quietPolls >= QUIET_POLLS_TO_STOP) {
            stop('settled');
            return;
          }
          schedule();
        });
    }

    document.addEventListener('visibilitychange', () => {
      if (document.hidden || inFlight) return;
      if (stopped) {
        // back on the tab: a job may have been queued from somewhere else
        stopped = false;
        quietPolls = 0;
        errors = 0;
      }
      tick();
    });

    schedule();
  }

  guardDoubleSubmit();
  filterBusyState();
  bulkPostSelection();
  slashToSearch();
  liveStatus();
})();
