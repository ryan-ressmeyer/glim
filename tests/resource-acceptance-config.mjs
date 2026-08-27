const MIB = 1024 * 1024;

export const ACCEPTANCE_DEFAULTS = Object.freeze({
  fileBytes: 64 * MIB,
  feedPosts: 150,
  timeoutMs: 120_000,
  maxHwmBytes: 384 * MIB,
  maxHwmGrowthBytes: 48 * MIB,
  maxPageBytes: 2 * MIB,
});

if (ACCEPTANCE_DEFAULTS.maxHwmGrowthBytes >= ACCEPTANCE_DEFAULTS.fileBytes) {
  throw new Error("default HWM growth ceiling must be smaller than the streamed file");
}
if (ACCEPTANCE_DEFAULTS.maxHwmBytes <= ACCEPTANCE_DEFAULTS.maxHwmGrowthBytes) {
  throw new Error("default absolute HWM ceiling must exceed the growth ceiling");
}

export function resolveAcceptanceConfiguration(environment) {
  const configuration = {
    fileBytes: Number(environment.GLIM_ACCEPT_FILE_BYTES ?? ACCEPTANCE_DEFAULTS.fileBytes),
    feedPosts: Number(environment.GLIM_ACCEPT_FEED_POSTS ?? ACCEPTANCE_DEFAULTS.feedPosts),
    timeoutMs: Number(environment.GLIM_ACCEPT_TIMEOUT_MS ?? ACCEPTANCE_DEFAULTS.timeoutMs),
    maxHwmBytes: Number(environment.GLIM_ACCEPT_MAX_HWM_BYTES ?? ACCEPTANCE_DEFAULTS.maxHwmBytes),
    maxHwmGrowthBytes: Number(environment.GLIM_ACCEPT_MAX_HWM_GROWTH_BYTES ?? ACCEPTANCE_DEFAULTS.maxHwmGrowthBytes),
    maxPageBytes: Number(environment.GLIM_ACCEPT_MAX_PAGE_BYTES ?? ACCEPTANCE_DEFAULTS.maxPageBytes),
  };
  for (const [name, value] of Object.entries(configuration)) {
    if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${name} must be a positive safe integer`);
  }
  return configuration;
}
