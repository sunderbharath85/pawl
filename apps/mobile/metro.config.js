// Metro config: Expo's defaults (monorepo + isolated bun installs are auto-detected, SDK 52+/54+).
// One override: tweetnacl has a guarded `require('crypto')` for Node. The native runtime has no Node
// standard library, so resolve it to an empty module (the app installs an expo-crypto PRNG instead).
const { getDefaultConfig } = require("expo/metro-config");

const config = getDefaultConfig(__dirname);
const upstreamResolve = config.resolver.resolveRequest;

config.resolver.resolveRequest = (context, moduleName, platform) => {
  if (moduleName === "crypto" && /[\/]tweetnacl[\/]/.test(context.originModulePath)) {
    return { type: "empty" };
  }
  return (upstreamResolve ?? context.resolveRequest)(context, moduleName, platform);
};

module.exports = config;
