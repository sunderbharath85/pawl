import { Stack } from "expo-router/stack";
import { useTheme } from "@/theme";

export default function Layout() {
  const t = useTheme();
  return (
    <Stack
      screenOptions={{
        headerLargeTitle: true,
        headerShadowVisible: false,
        headerLargeTitleShadowVisible: false,
        headerTransparent: process.env.EXPO_OS === "ios",
        headerBlurEffect: process.env.EXPO_OS === "ios" ? "systemChromeMaterial" : undefined,
        headerLargeStyle: { backgroundColor: "transparent" },
        headerTitleStyle: { color: t.text },
        headerTintColor: t.accent,
        headerBackButtonDisplayMode: "minimal",
        contentStyle: { backgroundColor: t.background },
      }}
    >
      <Stack.Screen name="index" options={{ title: "Approvals" }} />
    </Stack>
  );
}
