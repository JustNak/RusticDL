/**
 * Accessible chip/tag list for managing many string items (hosts, file extensions).
 * Supports filter, keyboard add/remove, and live region feedback.
 */

export type TagListNormalize = (raw: string) => string;

export interface TagListOptions {
  listEl: HTMLElement;
  inputEl: HTMLInputElement;
  addButton?: HTMLButtonElement | null;
  filterEl?: HTMLInputElement | null;
  countEl?: HTMLElement | null;
  feedbackEl?: HTMLElement | null;
  emptyEl?: HTMLElement | null;
  itemNoun: string;
  itemNounPlural?: string;
  normalize: TagListNormalize;
  invalidMessage?: (raw: string) => string;
  duplicateMessage?: (value: string) => string;
  emptyLabel?: string;
  /** Optional sort comparator; default localeCompare. */
  sort?: (a: string, b: string) => number;
  /** Fired after a user-driven add/remove/clear (not setValues). */
  onChange?: () => void;
}

export class TagListController {
  private items: string[] = [];
  private filter = '';
  private readonly opts: TagListOptions;

  constructor(opts: TagListOptions) {
    this.opts = opts;
    this.bind();
  }

  getValues(): string[] {
    return [...this.items];
  }

  setValues(values: string[]): void {
    const next: string[] = [];
    for (const raw of values) {
      const normalized = this.opts.normalize(raw);
      if (normalized && !next.includes(normalized)) next.push(normalized);
    }
    next.sort(this.opts.sort ?? ((a, b) => a.localeCompare(b)));
    this.items = next;
    this.render();
  }

  private bind(): void {
    const { inputEl, addButton, filterEl } = this.opts;

    addButton?.addEventListener('click', () => this.tryAddFromInput());

    inputEl.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        this.tryAddFromInput();
        return;
      }
      if (event.key === 'Backspace' && !inputEl.value) {
        const visible = this.visibleItems();
        if (visible.length === 0) return;
        // Remove the last *visible* chip (filter-aware), not lexicographic end of full list.
        event.preventDefault();
        this.remove(visible[visible.length - 1]!);
      }
    });

    // Paste bulk: "a, b, c" or multi-line hosts.
    inputEl.addEventListener('paste', (event) => {
      const text = event.clipboardData?.getData('text');
      if (!text || !/[\s,;]/.test(text)) return;
      event.preventDefault();
      const added = this.addMany(text);
      this.setFeedback(
        added > 0
          ? `Added ${added} ${added === 1 ? this.opts.itemNoun : this.plural()}.`
          : 'No new items from paste.',
      );
      inputEl.value = '';
      this.render();
      if (added > 0) this.opts.onChange?.();
    });

    filterEl?.addEventListener('input', () => {
      this.filter = filterEl.value.trim().toLowerCase();
      this.render();
    });
  }

  private tryAddFromInput(): void {
    const raw = this.opts.inputEl.value;
    if (!raw.trim()) return;

    // Match paste: multi-item delimiters bulk-add instead of a single invalid token.
    if (/[\s,;]/.test(raw.trim())) {
      const added = this.addMany(raw);
      this.setFeedback(
        added > 0
          ? `Added ${added} ${added === 1 ? this.opts.itemNoun : this.plural()}.`
          : `No new ${this.plural()} from input.`,
      );
      if (added > 0) {
        this.opts.inputEl.value = '';
        this.opts.inputEl.focus();
        this.render();
        this.opts.onChange?.();
      }
      return;
    }

    if (this.add(raw)) {
      this.opts.inputEl.value = '';
      this.opts.inputEl.focus();
    }
  }

  /** Split on whitespace/commas/semicolons and add each part. Returns count added. */
  private addMany(raw: string): number {
    const parts = raw.split(/[\s,;]+/).map((p) => p.trim()).filter(Boolean);
    let added = 0;
    for (const part of parts) {
      if (this.add(part, { silent: true })) added += 1;
    }
    return added;
  }

  add(raw: string, options: { silent?: boolean } = {}): boolean {
    const trimmed = raw.trim();
    if (!trimmed) return false;

    const normalized = this.opts.normalize(trimmed);
    if (!normalized) {
      if (!options.silent) {
        this.setFeedback(
          this.opts.invalidMessage?.(trimmed)
            ?? `"${trimmed}" is not a valid ${this.opts.itemNoun}.`,
        );
      }
      return false;
    }

    if (this.items.includes(normalized)) {
      if (!options.silent) {
        this.setFeedback(
          this.opts.duplicateMessage?.(normalized)
            ?? `"${normalized}" is already in the list.`,
        );
      }
      return false;
    }

    this.items.push(normalized);
    this.items.sort(this.opts.sort ?? ((a, b) => a.localeCompare(b)));
    if (!options.silent) {
      this.setFeedback(`Added ${normalized}.`);
      this.render();
      this.opts.onChange?.();
    }
    return true;
  }

  remove(value: string): void {
    const before = this.items.length;
    this.items = this.items.filter((item) => item !== value);
    if (this.items.length < before) {
      this.setFeedback(`Removed ${value}.`);
      this.render();
      this.opts.onChange?.();
    }
  }

  clear(): void {
    this.items = [];
    this.setFeedback(`Cleared all ${this.plural()}.`);
    this.render();
    this.opts.onChange?.();
  }

  private plural(): string {
    return this.opts.itemNounPlural ?? `${this.opts.itemNoun}s`;
  }

  private setFeedback(message: string): void {
    if (this.opts.feedbackEl) this.opts.feedbackEl.textContent = message;
  }

  private visibleItems(): string[] {
    if (!this.filter) return this.items;
    return this.items.filter((item) => item.toLowerCase().includes(this.filter));
  }

  private render(): void {
    const { listEl, countEl, emptyEl, emptyLabel, itemNoun } = this.opts;
    const visible = this.visibleItems();
    const total = this.items.length;

    if (countEl) {
      const noun = total === 1 ? itemNoun : this.plural();
      countEl.textContent = this.filter && visible.length !== total
        ? `${visible.length} of ${total} ${this.plural()}`
        : `${total} ${noun}`;
    }

    listEl.replaceChildren();

    if (total === 0) {
      if (emptyEl) {
        emptyEl.hidden = false;
        emptyEl.textContent = emptyLabel ?? `No ${this.plural()} yet.`;
      }
      listEl.setAttribute('aria-busy', 'false');
      return;
    }

    if (emptyEl) emptyEl.hidden = true;

    if (visible.length === 0) {
      const empty = document.createElement('li');
      empty.className = 'tag-list-empty';
      empty.textContent = `No ${this.plural()} match “${this.filter}”.`;
      listEl.append(empty);
      return;
    }

    const frag = document.createDocumentFragment();
    for (const value of visible) {
      const li = document.createElement('li');
      li.className = 'tag-chip';
      li.setAttribute('role', 'listitem');

      const label = document.createElement('span');
      label.className = 'tag-chip-label';
      label.textContent = value;
      label.title = value;

      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'tag-chip-remove';
      remove.setAttribute('aria-label', `Remove ${value}`);
      remove.title = `Remove ${value}`;
      remove.innerHTML =
        '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>';
      remove.addEventListener('click', () => this.remove(value));

      li.append(label, remove);
      frag.append(li);
    }
    listEl.append(frag);
  }
}

/**
 * Normalize a single file-extension token for the capture list.
 * Matches desktop `normalize_extensions` / `normalizeFileExtensions` rules so
 * load→edit→save round-trips do not drop previously accepted values (e.g. tar.gz).
 */
export function normalizeFileExtensionTag(raw: string): string {
  let extension = raw.trim().replace(/^\.+/, '').toLowerCase();
  if (extension === '7zip') extension = '7z';
  if (
    !extension
    || extension.includes('/')
    || extension.includes('\\')
    || extension.includes(' ')
    || extension.split('').every((c) => c === '.')
  ) {
    return '';
  }
  return extension;
}
