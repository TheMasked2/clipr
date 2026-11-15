<script lang="ts">
  let { checked = $bindable(false), disabled = false, label = '' }: { checked?: boolean; disabled?: boolean; label?: string } = $props();
</script>

<label class="toggle" class:off={disabled}>
  <input type="checkbox" bind:checked {disabled} aria-label={label || undefined} />
  <span class="track"><span class="knob"></span></span>
</label>

<style>
  .toggle { display: inline-block; cursor: pointer; flex: none; }
  .off { opacity: .5; cursor: default; }
  input { position: absolute; opacity: 0; width: 0; height: 0; }
  .track {
    display: block; width: 2.6rem; height: 1.5rem; border-radius: 999px;
    background: var(--surface-2); border: 1px solid var(--border); position: relative; transition: background .15s;
  }
  .knob {
    position: absolute; top: 2px; left: 2px; width: 1.1rem; height: 1.1rem; border-radius: 50%;
    background: var(--text-muted); transition: transform .15s, background .15s;
  }
  input:checked + .track { background: var(--accent); border-color: var(--accent); }
  input:checked + .track .knob { transform: translateX(1.1rem); background: var(--on-accent); }
  input:focus-visible + .track { outline: 2px solid var(--accent); outline-offset: 2px; }
</style>
