// Root: the phone peer (PhoneProvider), theme-aware status bar, and the root stack. Tabs live in (tabs); pairing,
// sessions and starting an agent are pushed on top of them.
import { DarkTheme, DefaultTheme, ThemeProvider } from "expo-router";
import { Stack } from "expo-router/stack";
import * as SplashScreen from "expo-splash-screen";
import { StatusBar } from "expo-status-bar";
import { useEffect } from "react";
import { Text, View } from "react-native";
import { PetSprite } from "@/components/pet/pet-sprite";
import { PhoneProvider } from "@/lib/phone/phone-provider";
import { useTheme } from "@/theme";

void SplashScreen.preventAutoHideAsync();

function Loading({ error }: { error: string | null }) {
  const t = useTheme();
  useEffect(() => {
    void SplashScreen.hideAsync();
  }, []);
  return (
    <View style={{ flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.background, gap: 16, padding: 32 }}>
      <PetSprite species="dog" activity={error ? "sad" : "napping"} size={140} />
      <Text selectable style={{ color: error ? t.danger : t.textMuted, fontSize: 16, textAlign: "center" }}>
        {error ?? "Waking up…"}
      </Text>
    </View>
  );
}

function Shell() {
  const t = useTheme();
  useEffect(() => {
    void SplashScreen.hideAsync();
  }, []);
  const base = t.scheme === "dark" ? DarkTheme : DefaultTheme;
  const navTheme = {
    ...base,
    colors: { ...base.colors, primary: t.accent, background: t.background, card: t.backgroundAlt, text: t.text, border: t.border },
  };
  return (
    <ThemeProvider value={navTheme}>
      <StatusBar style={t.scheme === "dark" ? "light" : "dark"} />
      <Stack screenOptions={{ headerBackButtonDisplayMode: "minimal", contentStyle: { backgroundColor: t.background } }}>
        <Stack.Screen name="(tabs)" options={{ headerShown: false }} />
        <Stack.Screen name="pair" options={{ presentation: "modal", title: "Pair with a computer" }} />
        <Stack.Screen name="start" options={{ presentation: "modal", title: "Start a session" }} />
        <Stack.Screen name="session/[id]" options={{ title: "Session" }} />
      </Stack>
    </ThemeProvider>
  );
}

export default function RootLayout() {
  return (
    <PhoneProvider fallback={(error) => <Loading error={error} />}>
      <Shell />
    </PhoneProvider>
  );
}
