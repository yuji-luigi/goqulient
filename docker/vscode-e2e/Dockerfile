# Runs the extension inside a real VS Code (@vscode/test-electron) on a virtual display (scripts/dev.sh vscode e2e).
FROM node:24-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      xvfb xauth libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 libgtk-3-0 libgbm1 libasound2 \
      libxkbfile1 libsecret-1-0 libxss1 libxshmfence1 imagemagick \
    && rm -rf /var/lib/apt/lists/*
