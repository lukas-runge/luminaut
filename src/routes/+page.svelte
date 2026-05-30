<script lang="ts">
  import { onMount, untrack } from "svelte";
  import {
    artNetListener,
    sAcnListener,
    stopListener,
    getNetworkInterfaces,
  } from "tauri/commands";
  import { getChannelFor } from "tauri/helpers";
  import type { dmxListenerChannelType, NetworkInterface } from "tauri/types";

  type Protocol = "sacn" | "artnet";

  // ── Config ──────────────────────────────────────────────────────────────────
  let protocol = $state<Protocol>("sacn");
  let interfaces = $state<NetworkInterface[]>([{ name: "Any", ip: "0.0.0.0" }]);
  let selectedIp = $state("0.0.0.0");

  // sACN universe 1–63999
  let sacnUniverse = $state(1);

  // Art-Net: net 0–127, subnet 0–15, universe 0–15
  let artnetNet = $state(0);
  let artnetSubnet = $state(0);
  let artnetUniverse = $state(0);

  // ── Runtime ─────────────────────────────────────────────────────────────────
  let listening = $state(false);
  let dmxData = $state<dmxListenerChannelType | null>(null);
  let errorMsg = $state<string | null>(null);

  const CHANNEL_COUNT = 512;

  const artnetPortAddress = $derived(
    ((artnetNet & 0x7f) << 8) | ((artnetSubnet & 0x0f) << 4) | (artnetUniverse & 0x0f)
  );

  const activePortAddress = $derived(
    protocol === "sacn" ? sacnUniverse : artnetPortAddress
  );

  const channels = $derived(
    dmxData ? dmxData.values.slice(0, CHANNEL_COUNT) : new Array(CHANNEL_COUNT).fill(0)
  );

  onMount(async () => {
    try {
      interfaces = await getNetworkInterfaces();
    } catch {
      // not running inside Tauri webview — keep default
    }
  });

  // ── Auto-restart when config changes while listening ─────────────────────────
  $effect(() => {
    // Track all config deps; listening is intentionally untracked
    protocol; sacnUniverse; artnetNet; artnetSubnet; artnetUniverse; selectedIp;
    untrack(() => { if (listening) restart(); });
  });

  // ── Listener lifecycle ───────────────────────────────────────────────────────
  async function doStop() {
    listening = false;
    dmxData = null;
    try { await stopListener(); } catch { /* ignore when not in Tauri */ }
  }

  async function startListening() {
    errorMsg = null;
    dmxData = null;
    listening = true;

    // Capture port address at start time to match incoming frames
    const targetPort = activePortAddress;

    const channel = getChannelFor<dmxListenerChannelType>((msg) => {
      if (msg.error) { errorMsg = msg.error; listening = false; return; }
      if (msg.universe === targetPort) dmxData = msg;
    });

    try {
      if (protocol === "sacn") {
        await sAcnListener(sacnUniverse, selectedIp, channel);
      } else {
        await artNetListener(artnetNet, artnetSubnet, artnetUniverse, selectedIp, channel);
      }
    } catch (e) {
      errorMsg = String(e);
      listening = false;
    }
  }

  async function stopListening() {
    errorMsg = null;
    await doStop();
  }

  async function restart() {
    await doStop();
    await startListening();
  }

  // ── Helpers ──────────────────────────────────────────────────────────────────
  function clamp(v: number, min: number, max: number) {
    return Math.max(min, Math.min(max, isNaN(v) ? min : v));
  }

  function channelColor(value: number): string {
    if (value === 0) return "bg-base-300";
    if (value < 64) return "bg-info/40";
    if (value < 128) return "bg-info/70";
    if (value < 192) return "bg-primary/80";
    return "bg-primary";
  }
</script>

<div class="min-h-screen bg-base-200 flex flex-col" data-theme="dark">

  <!-- ── Header ─────────────────────────────────────────────────────────────── -->
  <header class="navbar bg-base-100 shadow-lg px-4">
    <div class="flex-1">
      <span class="text-xl font-bold tracking-widest text-primary uppercase">Luminaut</span>
      <span class="ml-3 text-xs text-base-content/40 uppercase tracking-widest">DMX Monitor</span>
    </div>
    {#if listening}
      <span class="badge badge-success gap-2 animate-pulse">
        <span class="w-2 h-2 rounded-full bg-success-content inline-block"></span>
        Live
      </span>
    {:else}
      <span class="badge badge-ghost gap-2">
        <span class="w-2 h-2 rounded-full bg-base-content/30 inline-block"></span>
        Idle
      </span>
    {/if}
  </header>

  <div class="p-4 flex flex-col gap-3">

    <!-- ── Controls ───────────────────────────────────────────────────────────── -->
    <div class="card bg-base-100 shadow">
      <div class="card-body py-3 px-4 flex flex-row flex-wrap gap-x-6 gap-y-3 items-end">

        <!-- Protocol -->
        <div class="flex flex-col gap-1">
          <span class="text-xs text-base-content/50 uppercase tracking-wider">Protocol</span>
          <div class="join">
            <button
              class="btn btn-sm join-item {protocol === 'sacn' ? 'btn-primary' : 'btn-ghost'}"
              onclick={() => (protocol = "sacn")}
            >sACN</button>
            <button
              class="btn btn-sm join-item {protocol === 'artnet' ? 'btn-primary' : 'btn-ghost'}"
              onclick={() => (protocol = "artnet")}
            >Art-Net</button>
          </div>
        </div>

        <div class="divider divider-horizontal m-0 self-stretch hidden sm:flex"></div>

        <!-- Interface -->
        <div class="flex flex-col gap-1">
          <span class="text-xs text-base-content/50 uppercase tracking-wider">Interface</span>
          <select
            class="select select-sm font-mono min-w-48 bg-base-300 border-0"
            bind:value={selectedIp}
          >
            {#each interfaces as iface}
              <option value={iface.ip}>{iface.name}</option>
            {/each}
          </select>
        </div>

        <div class="divider divider-horizontal m-0 self-stretch hidden sm:flex"></div>

        <!-- Universe controls -->
        {#if protocol === "sacn"}
          <div class="flex flex-col gap-1">
            <span class="text-xs text-base-content/50 uppercase tracking-wider">Universe</span>
            <div class="join">
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (sacnUniverse = clamp(sacnUniverse - 1, 1, 63999))}>−</button>
              <input
                class="input input-sm join-item w-20 font-mono text-center bg-base-300 border-0"
                type="number" min="1" max="63999" onfocus={(e) => { const el = e.currentTarget; setTimeout(() => el.select(), 0); }}
                value={sacnUniverse}
                oninput={(e) => (sacnUniverse = clamp(parseInt(e.currentTarget.value, 10), 1, 63999))}
              />
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (sacnUniverse = clamp(sacnUniverse + 1, 1, 63999))}>+</button>
            </div>
          </div>
        {:else}
          <!-- Art-Net: Net / SubNet / Universe + computed port address -->
          <div class="flex flex-col gap-1">
            <span class="text-xs text-base-content/50 uppercase tracking-wider">Net</span>
            <div class="join">
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (artnetNet = clamp(artnetNet - 1, 0, 127))}>−</button>
              <input
                class="input input-sm join-item w-16 font-mono text-center bg-base-300 border-0"
                type="number" min="0" max="127" onfocus={(e) => { const el = e.currentTarget; setTimeout(() => el.select(), 0); }}
                value={artnetNet}
                oninput={(e) => (artnetNet = clamp(parseInt(e.currentTarget.value, 10), 0, 127))}
              />
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (artnetNet = clamp(artnetNet + 1, 0, 127))}>+</button>
            </div>
          </div>

          <div class="flex flex-col gap-1">
            <span class="text-xs text-base-content/50 uppercase tracking-wider">Sub-Net</span>
            <div class="join">
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (artnetSubnet = clamp(artnetSubnet - 1, 0, 15))}>−</button>
              <input
                class="input input-sm join-item w-16 font-mono text-center bg-base-300 border-0"
                type="number" min="0" max="15" onfocus={(e) => { const el = e.currentTarget; setTimeout(() => el.select(), 0); }}
                value={artnetSubnet}
                oninput={(e) => (artnetSubnet = clamp(parseInt(e.currentTarget.value, 10), 0, 15))}
              />
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (artnetSubnet = clamp(artnetSubnet + 1, 0, 15))}>+</button>
            </div>
          </div>

          <div class="flex flex-col gap-1">
            <span class="text-xs text-base-content/50 uppercase tracking-wider">Universe</span>
            <div class="join">
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (artnetUniverse = clamp(artnetUniverse - 1, 0, 15))}>−</button>
              <input
                class="input input-sm join-item w-16 font-mono text-center bg-base-300 border-0"
                type="number" min="0" max="15" onfocus={(e) => { const el = e.currentTarget; setTimeout(() => el.select(), 0); }}
                value={artnetUniverse}
                oninput={(e) => (artnetUniverse = clamp(parseInt(e.currentTarget.value, 10), 0, 15))}
              />
              <button class="btn btn-sm join-item btn-square bg-base-300 border-0 hover:bg-base-200"
                onclick={() => (artnetUniverse = clamp(artnetUniverse + 1, 0, 15))}>+</button>
            </div>
          </div>

          <div class="flex flex-col gap-1 justify-end">
            <span class="text-xs text-base-content/30 uppercase tracking-wider">Port</span>
            <span class="font-mono text-sm text-base-content/60 h-8 flex items-center px-1">
              {artnetPortAddress}
            </span>
          </div>
        {/if}

        <div class="flex-1"></div>

        <!-- Listen / Stop -->
        {#if listening}
          <button class="btn btn-error btn-sm gap-2" onclick={stopListening}>
            <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 20 20">
              <rect x="4" y="4" width="12" height="12" rx="1"/>
            </svg>
            Stop
          </button>
        {:else}
          <button class="btn btn-primary btn-sm gap-2" onclick={startListening}>
            <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 20 20">
              <path d="M6.5 5.5l8 4.5-8 4.5V5.5z"/>
            </svg>
            Listen
          </button>
        {/if}

      </div>
    </div>

    <!-- ── Error ───────────────────────────────────────────────────────────────── -->
    {#if errorMsg}
      <div class="alert alert-error text-sm py-2">
        <svg class="w-4 h-4 shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2"
            d="M12 9v2m0 4h.01M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/>
        </svg>
        <span>{errorMsg}</span>
        <button class="btn btn-xs btn-ghost ml-auto" onclick={() => (errorMsg = null)}>✕</button>
      </div>
    {/if}

    <!-- ── Universe panel ─────────────────────────────────────────────────────── -->
    <div class="card bg-base-100 shadow">
      <div class="card-body p-4 gap-3">

        <!-- Panel header -->
        <div class="flex items-baseline gap-2 flex-wrap">
          <h2 class="card-title text-base font-semibold">
            {#if protocol === "sacn"}
              Universe <span class="badge badge-primary badge-lg font-mono ml-1">{sacnUniverse}</span>
            {:else}
              Port <span class="badge badge-primary badge-lg font-mono ml-1">{artnetPortAddress}</span>
              <span class="text-xs text-base-content/40 font-mono">
                Net&nbsp;{artnetNet} · Sub&nbsp;{artnetSubnet} · Uni&nbsp;{artnetUniverse}
              </span>
            {/if}
          </h2>
          {#if dmxData && protocol === "sacn"}
            <span class="text-xs text-base-content/40 font-mono ml-auto">priority {dmxData.priority}</span>
          {/if}
        </div>

        {#if !listening}
          <div class="flex flex-col items-center justify-center py-16 gap-3 text-base-content/30">
            <svg class="w-10 h-10" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5"
                d="M9 3H5a2 2 0 00-2 2v4m6-6h10a2 2 0 012 2v4M9 3v18m0 0h10a2 2 0 002-2V9M9 21H5a2 2 0 01-2-2V9m0 0h18"/>
            </svg>
            <span class="text-sm">Press Listen to start monitoring</span>
          </div>
        {:else if !dmxData}
          <div class="flex flex-col items-center justify-center py-16 gap-3 text-base-content/30">
            <span class="loading loading-dots loading-md"></span>
            <span class="text-sm">
              {#if protocol === "sacn"}
                Waiting for packets on universe {sacnUniverse}…
              {:else}
                Waiting for packets on port address {artnetPortAddress}…
              {/if}
            </span>
          </div>
        {:else}
          <!-- Channel grid -->
          <div class="grid gap-1" style="grid-template-columns: repeat(auto-fill, minmax(64px, 1fr));">
            {#each channels as value, i}
              <div class="flex flex-col items-center gap-1">
                <div
                  class="w-full flex flex-col-reverse rounded overflow-hidden bg-base-300 relative"
                  style="aspect-ratio: 1 / 1;"
                >
                  <div class="w-full {channelColor(value)}" style="height: {(value / 255) * 100}%"></div>
                  <span
                    class="absolute bottom-0 inset-x-0 text-center text-sm font-mono leading-none pb-1
                      {value > 180 ? 'text-base-100' : 'text-base-content/70'}"
                  >{value}</span>
                </div>
                <span class="text-xs text-base-content/40 font-mono leading-none">{i + 1}</span>
              </div>
            {/each}
          </div>
        {/if}

      </div>
    </div>

  </div>
</div>
