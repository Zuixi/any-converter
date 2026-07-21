import type { Metadata } from "next";

import { AppProviders } from "@/components/app-providers";
import { Navigation } from "@/components/navigation";
import "./globals.css";

export const metadata: Metadata = {
  title: "any-converter web",
  description: "Web interface for any-converter",
  icons: {
    icon: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Crect width='32' height='32' rx='6' fill='%23000'/%3E%3Cpath d='M8 17h10l-3 3 2 2 7-7-7-7-2 2 3 3H8z' fill='%23fff'/%3E%3C/svg%3E",
  },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body className="min-h-screen bg-background font-sans antialiased">
        <AppProviders>
          <Navigation />
          <main className="container py-6">{children}</main>
        </AppProviders>
      </body>
    </html>
  );
}
