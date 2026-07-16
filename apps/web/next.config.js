/** @type {import('next').NextConfig} */
const nextConfig = {
  transpilePackages: ["@any-converter/ui", "@any-converter/shared", "@any-converter/core", "@any-converter/views"],
  webpack: (config, { isServer }) => {
    if (isServer) {
      config.externals.push("@any-converter/bridge");
    }
    return config;
  },
};

module.exports = nextConfig;
