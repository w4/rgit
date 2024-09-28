#!/usr/bin/env bash

if [ -z ${REFRESH_INTERVAL+x} ];
then 
	./rgit "[::]:8000" /git -d /tmp/rgit-cache.db;
else
	./rgit "[::]:8000" /git -d /tmp/rgit-cache.db --refresh-interval "$REFRESH_INTERVAL";
fi
