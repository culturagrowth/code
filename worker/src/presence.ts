/** Shared heartbeat freshness policy; remote timestamps never determine whether a device is online. */
export const PRESENCE_TTL_MS = 90_000;
/** Clients receive all crew snapshots in this single periodic authenticated request. */
export const PRESENCE_HEARTBEAT_MS = 30_000;
