import native from 'librustdesk_native_har.so';

export function runtimeStartCaptureInputProbe(): string { return native.runtimeStartCaptureInputProbe(); }
export function runtimePollCaptureInputProbe(): string { return native.runtimePollCaptureInputProbe(); }
export function runtimeStopCaptureInputProbe(): string { return native.runtimeStopCaptureInputProbe(); }
export function runtimeStartEncoderInputProbe(): string { return native.runtimeStartEncoderInputProbe(); }
export function runtimePollEncoderInputProbe(): string { return native.runtimePollEncoderInputProbe(); }
export function runtimeStartGlesBufferProbe(): string { return native.runtimeStartGlesBufferProbe(); }
export function runtimePollGlesBufferProbe(): string { return native.runtimePollGlesBufferProbe(); }
export function runtimeStartNativeBufferProbe(): string { return native.runtimeStartNativeBufferProbe(); }
export function runtimePollNativeBufferProbe(): string { return native.runtimePollNativeBufferProbe(); }

/**
 * Adopts the native XComponent that arrives with the Surface's `onLoad`.
 *
 * The ArkUI node route is closed on this platform: nothing `getFrameNodeById`
 * returns is accepted as an XComponent, so the native peer the framework hands
 * this module is held instead. Returns 0 once it is held.
 */
export function xcomponentAttach(context: object): number {
  return native.xcomponentAttach(context);
}

/**
 * Declares the rate the video Surface is produced at, through that component.
 *
 * Returns the platform's result code: 0 means the expectation was accepted.
 */
export function xcomponentSetFrameRateRangeNative(
  expected: number,
  min: number,
  max: number,
): number {
  return native.xcomponentSetFrameRateRangeNative(expected, min, max);
}

// Explicitly enabled view-only host. Per-peer local approval and system screen
// recording consent remain mandatory; no input/clipboard/audio/file support.
export function engineHostStart(width: number, height: number, fps: number): string {
  return native.engineHostStart(width, height, fps);
}
export function engineHostSnapshot(id: string): string { return native.engineHostSnapshot(id); }
export function engineHostApprove(id: string, requestId: string, allow: boolean): boolean {
  return native.engineHostApprove(id, requestId, allow);
}
export function engineHostRequestClose(id: string): void { native.engineHostRequestClose(id); }
export function engineHostClose(id: string): Promise<void> {
  native.engineHostRequestClose(id);
  return native.engineHostClose(id);
}
// JSON: id + peer_signing_key_base64 (PUBLIC key only), for out-of-band trust.
export function engineHostIdentity(): string { return native.engineHostIdentity(); }

// Modern-only entry points. Normal navigation keeps the XComponent mounted until
// engineClose resolves. A platform Surface-destroy callback must immediately
// request close; native retains its explicit NativeWindow reference until close.
// Invoke only after privacy consent, with trusted Ability.filesDir. The returned
// ID is authoritative; UI must not generate/substitute its own protocol identity.
export function engineInitialize(appDir: string): string { return native.engineInitialize(appDir); }

export function engineConnect(surfaceId: string, address: string, username: string,
  localId: string, localName: string, password: string, fps: number): string {
  return native.engineConnect(surfaceId, address, username, localId, localName, password, fps);
}

export function engineSnapshot(id: string): string { return native.engineSnapshot(id); }

// Strict peer pin obtained out of band; never use an hbbs key or auto-TOFU.
export function engineConnectVerified(surfaceId: string, address: string, username: string,
  localId: string, localName: string, password: string, requestedFps: number,
  expectedPeerId: string, peerSigningKeyBase64: string): string {
  return native.engineConnectVerified(surfaceId, address, username, localId, localName,
    password, requestedFps, expectedPeerId, peerSigningKeyBase64);
}

// Strict original hbbs/hbbr ID route. Core reads the initialized app's configured
// rendezvous server/key and requires both server-signed and peer-signed identity.
export function engineConnectId(surfaceId: string, peerId: string, localId: string,
  localName: string, password: string, requestedFps: number): string {
  return native.engineConnectId(surfaceId, peerId, localId, localName, password, requestedFps);
}

export function engineMouse(id: string, kind: number, button: number, x: number, y: number): boolean {
  return native.engineMouse(id, kind, button, x, y);
}

// Cancellation enters native synchronously, before scheduling any Promise work.
// Repeated lifecycle requests are safe and never release the NativeWindow.
export function engineRequestClose(id: string): void { native.engineRequestClose(id); }

export function engineClose(id: string): Promise<void> {
  native.engineRequestClose(id);
  return native.engineClose(id);
}

export function engineCapabilities(): Promise<string> { return native.engineCapabilities(); }

// Legacy declarations/wrappers below are retained only for unloaded old UI
// compilation. The modern native binary does NOT initialize/export that runtime.
export function backendSummary(): string {
  return native.backendSummary();
}

export function buildMarker(): string {
  return native.buildMarker();
}

export function healthcheck(): string {
  return native.healthcheck();
}

export function controlledServerStart(configJson: string): Promise<string> {
  return native.controlledServerStart(configJson);
}
export function controlledServerStop(): Promise<string> { return native.controlledServerStop(); }
export function controlledServerGetStatus(): string { return native.controlledServerGetStatus(); }
export function controlledIncomingPoll(limit: number): string { return native.controlledIncomingPoll(limit); }
export function controlledIncomingResolve(requestId: string, accepted: boolean): string { return native.controlledIncomingResolve(requestId, accepted); }
export function controlledIncomingSetPermission(requestId: string, permission: string, enabled: boolean): string { return native.controlledIncomingSetPermission(requestId, permission, enabled); }
export function controlledPasswordGet(): string { return native.controlledPasswordGet(); }
export function controlledPasswordRefresh(): string { return native.controlledPasswordRefresh(); }
export function controlledPasswordSetPermanent(password: string): string { return native.controlledPasswordSetPermanent(password); }
export function controlledPasswordSetVerificationMethod(method: string): string {
  return native.controlledPasswordSetVerificationMethod(method);
}
export function controlledSettingsGet(): string { return native.controlledSettingsGet(); }
export function controlledSettingSet(key: string, value: string): string {
  return native.controlledSettingSet(key, value);
}
export function controlledTwoFactorGet(): string { return native.controlledTwoFactorGet(); }
export function controlledTwoFactorBegin(): string { return native.controlledTwoFactorBegin(); }
export function controlledTwoFactorVerify(code: string): string { return native.controlledTwoFactorVerify(code); }
export function controlledTwoFactorDisable(): string { return native.controlledTwoFactorDisable(); }
export function controlledTwoFactorRemoveTrustedDevices(hwidsJson: string): string {
  return native.controlledTwoFactorRemoveTrustedDevices(hwidsJson);
}
export function controlledTwoFactorClearTrustedDevices(): string {
  return native.controlledTwoFactorClearTrustedDevices();
}
export function controlledScreenConfigure(configJson: string): string { return native.controlledScreenConfigure(configJson); }
export function controlledScreenCaptureStart(configJson: string): string { return native.controlledScreenCaptureStart(configJson); }
export function controlledScreenCaptureStop(): string { return native.controlledScreenCaptureStop(); }
export function controlledScreenCaptureGetStatus(): string { return native.controlledScreenCaptureGetStatus(); }

export function inputInterceptorPollEvents(limit: number): string {
  return native.inputInterceptorPollEvents(limit);
}

export function inputInterceptorStart(): string {
  return native.inputInterceptorStart();
}

export function inputInterceptorStop(): string {
  return native.inputInterceptorStop();
}

export function nativeVersion(): string {
  return native.nativeVersion();
}

export function runtimeGetApiServer(): string {
  return native.runtimeGetApiServer();
}

export function runtimeGetAccountState(): string {
  return native.runtimeGetAccountState();
}

export function runtimeGetAccountLoginOptions(): string {
  return native.runtimeGetAccountLoginOptions();
}

export function runtimeStartAccountLoginOptions(): string {
  return native.runtimeStartAccountLoginOptions();
}

export function runtimePollAccountLoginOptions(): string {
  return native.runtimePollAccountLoginOptions();
}

export function runtimeStartAccountLogin(payloadJson: string): string {
  return native.runtimeStartAccountLogin(payloadJson);
}

export function runtimeStartAccountOidc(provider: string): string {
  return native.runtimeStartAccountOidc(provider);
}

export function runtimePollAccountOidc(): string {
  return native.runtimePollAccountOidc();
}

export function runtimeCancelAccountOidc(): string {
  return native.runtimeCancelAccountOidc();
}

export function runtimeStartAccountVerification(payloadJson: string): string {
  return native.runtimeStartAccountVerification(payloadJson);
}

export function runtimePollAccountAction(): string {
  return native.runtimePollAccountAction();
}

export function runtimeAccountLogout(): string {
  return native.runtimeAccountLogout();
}

export function runtimeCancelAccountChallenge(): string {
  return native.runtimeCancelAccountChallenge();
}

export function runtimeStartAddressBookSync(): string {
  return native.runtimeStartAddressBookSync();
}

export function runtimePollAddressBookSync(): string {
  return native.runtimePollAddressBookSync();
}

export function runtimeGetDecoderCapabilities(): string {
  return native.runtimeGetDecoderCapabilities();
}

export function runtimeGetServerConfig(): string {
  return native.runtimeGetServerConfig();
}

export function runtimeInit(appDir: string, deviceName: string): string {
  return native.runtimeInit(appDir, deviceName);
}

export function runtimeListAddressBookPeers(): string {
  return native.runtimeListAddressBookPeers();
}

export function runtimeListFavorites(): string {
  return native.runtimeListFavorites();
}

export function runtimeListLanPeers(): string {
  return native.runtimeListLanPeers();
}

export function runtimeListLocalDir(path: string): string {
  return native.runtimeListLocalDir(path);
}

export function runtimeListRecentPeers(): string {
  return native.runtimeListRecentPeers();
}

export function runtimeQueryPeerOnlineStates(peerIdsJson: string): Promise<string> {
  return native.runtimeQueryPeerOnlineStates(peerIdsJson);
}

export function runtimeProbePeerDirectStates(targetsJson: string): Promise<string> {
  return native.runtimeProbePeerDirectStates(targetsJson);
}

export function runtimeRemoveDiscoveredPeer(peerId: string): string {
  return native.runtimeRemoveDiscoveredPeer(peerId);
}

export function runtimeRemoveRecentPeer(peerId: string): string {
  return native.runtimeRemoveRecentPeer(peerId);
}

export function runtimeScanLanPeers(): Promise<string> {
  return native.runtimeScanLanPeers();
}

export function runtimeSetFavorite(peerId: string, favorite: boolean): string {
  return native.runtimeSetFavorite(peerId, favorite);
}

export function runtimeSetServerConfig(configJson: string): string {
  return native.runtimeSetServerConfig(configJson);
}

export function runtimeTestServerConfig(configJson: string): Promise<string> {
  return native.runtimeTestServerConfig(configJson);
}

export function sessionAdd(sessionId: string, peerTarget: string, optionsJson: string): string {
  return native.sessionAdd(sessionId, peerTarget, optionsJson);
}

export function sessionAlternativeCodecs(sessionId: string): string {
  return native.sessionAlternativeCodecs(sessionId);
}

export function sessionBindSurface(sessionId: string, display: number, surfaceId: string): string {
  return native.sessionBindSurface(sessionId, display, surfaceId);
}

export function sessionGetSoftwarePresentState(sessionId: string, display: number): string {
  return native.sessionGetSoftwarePresentState(sessionId, display);
}

export function sessionGetFrameRateSnapshot(sessionId: string, display: number): string {
  return native.sessionGetFrameRateSnapshot(sessionId, display);
}

export function sessionGetNativeDecoderSnapshot(sessionId: string, display: number): string {
  return native.sessionGetNativeDecoderSnapshot(sessionId, display);
}

export function sessionCancelJob(sessionId: string, actId: number): string {
  return native.sessionCancelJob(sessionId, actId);
}

export function sessionChangeResolution(sessionId: string, display: number, width: number, height: number): string {
  return native.sessionChangeResolution(sessionId, display, width, height);
}

export function sessionClose(sessionId: string): string {
  return native.sessionClose(sessionId);
}

export function sessionEnterOrLeave(sessionId: string, enter: boolean): string {
  return native.sessionEnterOrLeave(sessionId, enter);
}

export function sessionGetConnToken(sessionId: string): string | null {
  return native.sessionGetConnToken(sessionId);
}

export function sessionGetEnableTrustedDevices(sessionId: string): boolean {
  return native.sessionGetEnableTrustedDevices(sessionId);
}

export function sessionGetImageQuality(sessionId: string): string | null {
  return native.sessionGetImageQuality(sessionId);
}

export function sessionGetRemoteAudioState(sessionId: string): string {
  return native.sessionGetRemoteAudioState(sessionId);
}

export function sessionGetRgbaSize(sessionId: string, display: number): number {
  return native.sessionGetRgbaSize(sessionId, display);
}

export function sessionGetToggleOption(sessionId: string, arg: string): boolean {
  return native.sessionGetToggleOption(sessionId, arg);
}

export function sessionHandleFlutterKeyEvent(sessionId: string, keyJson: string): string {
  return native.sessionHandleFlutterKeyEvent(sessionId, keyJson);
}

export function sessionInputKey(sessionId: string, keyJson: string): string {
  return native.sessionInputKey(sessionId, keyJson);
}

export function sessionInputString(sessionId: string, value: string): string {
  return native.sessionInputString(sessionId, value);
}

export function sessionLogin(sessionId: string, loginJson: string): string {
  return native.sessionLogin(sessionId, loginJson);
}

export function sessionNextRgba(sessionId: string, display: number): void {
  native.sessionNextRgba(sessionId, display);
}

export function sessionOnWaitingForImageDialogShow(sessionId: string): string {
  return native.sessionOnWaitingForImageDialogShow(sessionId);
}

export function sessionPeerOption(sessionId: string, name: string, value: string): void {
  native.sessionPeerOption(sessionId, name, value);
}

export function sessionPollEvents(sessionId: string, limit: number): string {
  return native.sessionPollEvents(sessionId, limit);
}

export function sessionReadRemoteDir(sessionId: string, path: string, includeHidden: boolean): string {
  return native.sessionReadRemoteDir(sessionId, path, includeHidden);
}

export function sessionReconnect(sessionId: string, forceRelay: boolean): string {
  return native.sessionReconnect(sessionId, forceRelay);
}

export function sessionRefresh(sessionId: string, display: number): string {
  return native.sessionRefresh(sessionId, display);
}

export function sessionSetVideoPaused(sessionId: string, paused: boolean): string {
  return native.sessionSetVideoPaused(sessionId, paused);
}

export function sessionSend2Fa(sessionId: string, code: string, trustThisDevice: boolean): string {
  return native.sessionSend2Fa(sessionId, code, trustThisDevice);
}

export function sessionSendChat(sessionId: string, text: string): string {
  return native.sessionSendChat(sessionId, text);
}

export function sessionSendClipboard(sessionId: string, content: string): string {
  return native.sessionSendClipboard(sessionId, content);
}

export function sessionSendClipboardImage(sessionId: string, image: Uint8Array): string {
  return native.sessionSendClipboardImage(sessionId, image);
}

export function sessionSendClipboardFiles(sessionId: string, pathsJson: string): string {
  return native.sessionSendClipboardFiles(sessionId, pathsJson);
}

export function sessionSetClipboardFileRoot(sessionId: string, root: string): string {
  return native.sessionSetClipboardFileRoot(sessionId, root);
}

export function sessionSendFiles(sessionId: string, actId: number, path: string, to: string, fileNum: number, includeHidden: boolean, isRemote: boolean, isDir: boolean): string {
  return native.sessionSendFiles(sessionId, actId, path, to, fileNum, includeHidden, isRemote, isDir);
}

export function sessionSendMouse(sessionId: string, mouseJson: string): string {
  return native.sessionSendMouse(sessionId, mouseJson);
}

export function sessionSendMouseEvent(sessionId: string, eventKind: number, button: number, x: number, y: number,
  relativeMarker: number): boolean {
  return native.sessionSendMouseEvent(sessionId, eventKind, button, x, y, relativeMarker);
}

// Delivery gates for local input. The boolean mouse entry point cannot explain a
// refusal, so this exposes the same gates the injector enforces.
export function sessionInputDeliveryStatus(sessionId: string): string {
  return native.sessionInputDeliveryStatus(sessionId);
}

export function sessionSendNote(sessionId: string, note: string): void {
  native.sessionSendNote(sessionId, note);
}

export function sessionSendPointer(sessionId: string, pointerJson: string): string {
  return native.sessionSendPointer(sessionId, pointerJson);
}

export function sessionTakeClipboard(sessionId: string): string {
  return native.sessionTakeClipboard(sessionId);
}

export function sessionTakeClipboardImage(sessionId: string): Uint8Array {
  return native.sessionTakeClipboardImage(sessionId);
}

export function sessionSetCodecPreference(sessionId: string, codec: string): string {
  return native.sessionSetCodecPreference(sessionId, codec);
}

export function sessionSetCommon(sessionId: string, key: string, value: string): string {
  // The native side reports whether the decision reached the session; dropping
  // that made a lost key indistinguishable from an applied one.
  return native.sessionSetCommon(sessionId, key, value);
}

export function sessionSetConfirmOverrideFile(sessionId: string, actId: number, fileNum: number, needOverride: boolean, remember: boolean, isUpload: boolean): string {
  return native.sessionSetConfirmOverrideFile(sessionId, actId, fileNum, needOverride, remember, isUpload);
}

export function sessionSetCustomFps(sessionId: string, fps: number): string {
  return native.sessionSetCustomFps(sessionId, fps);
}

export function sessionSetCustomImageQuality(sessionId: string, value: number): void {
  native.sessionSetCustomImageQuality(sessionId, value);
}

export function sessionSetImageQuality(sessionId: string, value: string): string {
  return native.sessionSetImageQuality(sessionId, value);
}

export function sessionSetShowRemoteCursor(sessionId: string, enabled: boolean): string {
  return native.sessionSetShowRemoteCursor(sessionId, enabled);
}

export function sessionSetSize(sessionId: string, display: number, width: number, height: number): string {
  return native.sessionSetSize(sessionId, display, width, height);
}

export function sessionStart(sessionId: string): string {
  return native.sessionStart(sessionId);
}

export function sessionSetViewOnly(sessionId: string, enabled: boolean): string {
  return native.sessionSetViewOnly(sessionId, enabled);
}

export function sessionSwitchDisplay(sessionId: string, displaysJson: string): string {
  return native.sessionSwitchDisplay(sessionId, displaysJson);
}

export function sessionTakeRgbaFrame(sessionId: string, display: number): Uint8Array {
  return native.sessionTakeRgbaFrame(sessionId, display);
}

export function sessionToggleOption(sessionId: string, arg: string): void {
  native.sessionToggleOption(sessionId, arg);
}

export function transferNextJobId(): number {
  return native.transferNextJobId();
}
