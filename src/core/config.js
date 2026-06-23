const fs = require('fs');
const path = require('path');
const { validateCronExpression } = require('cron');

class ConfigValidationError extends Error {
  constructor(filePath, issues) {
    const normalizedPath = filePath || 'config.json';
    const normalizedIssues = Array.isArray(issues) ? issues.filter(Boolean) : [];
    const preview = normalizedIssues.slice(0, 8).map((issue) => `- ${issue}`).join('\n');
    const suffix = normalizedIssues.length > 8
      ? `\n- +${normalizedIssues.length - 8} more issue(s)`
      : '';
    super(`Config file ${normalizedPath} is invalid:\n${preview}${suffix}`);
    this.name = 'ConfigValidationError';
    this.filePath = normalizedPath;
    this.issues = normalizedIssues;
  }
}

class ConfigParseError extends Error {
  constructor(filePath, cause) {
    super(`Config file ${filePath || 'config.json'} contains invalid JSON: ${cause?.message || 'unknown parse error'}`);
    this.name = 'ConfigParseError';
    this.filePath = filePath || 'config.json';
    this.cause = cause || null;
  }
}

function isPlainObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function addIssue(issues, field, message) {
  issues.push(`${field} ${message}`);
}

function validateBoolean(value, field, issues) {
  if (value === undefined) return;
  if (typeof value !== 'boolean') {
    addIssue(issues, field, 'must be a boolean.');
  }
}

function validateString(value, field, issues, { allowEmpty = true } = {}) {
  if (value === undefined) return;
  if (typeof value !== 'string') {
    addIssue(issues, field, 'must be a string.');
    return;
  }
  if (!allowEmpty && value.trim().length === 0) {
    addIssue(issues, field, 'must not be empty.');
  }
}

function validateIdString(value, field, issues, { allowEmpty = false } = {}) {
  if (value === undefined) return;
  if (typeof value !== 'string') {
    addIssue(issues, field, 'must be a string ID.');
    return;
  }
  if (!allowEmpty && value.trim().length === 0) {
    addIssue(issues, field, 'must not be empty.');
  }
}

function validateStringArray(value, field, issues, { allowEmptyStrings = false } = {}) {
  if (value === undefined) return;
  if (!Array.isArray(value)) {
    addIssue(issues, field, 'must be an array.');
    return;
  }
  value.forEach((item, index) => {
    if (typeof item !== 'string') {
      addIssue(issues, `${field}[${index}]`, 'must be a string.');
      return;
    }
    if (!allowEmptyStrings && item.trim().length === 0) {
      addIssue(issues, `${field}[${index}]`, 'must not be empty.');
    }
  });
}

function validateInteger(value, field, issues, { min = null, max = null } = {}) {
  if (value === undefined) return;
  if (!Number.isInteger(value)) {
    addIssue(issues, field, 'must be an integer.');
    return;
  }
  if (min !== null && value < min) {
    addIssue(issues, field, `must be >= ${min}.`);
  }
  if (max !== null && value > max) {
    addIssue(issues, field, `must be <= ${max}.`);
  }
}

function validateNumber(value, field, issues, { min = null, max = null } = {}) {
  if (value === undefined) return;
  if (typeof value !== 'number' || Number.isNaN(value) || !Number.isFinite(value)) {
    addIssue(issues, field, 'must be a finite number.');
    return;
  }
  if (min !== null && value < min) {
    addIssue(issues, field, `must be >= ${min}.`);
  }
  if (max !== null && value > max) {
    addIssue(issues, field, `must be <= ${max}.`);
  }
}

function validateTimeZone(value, field, issues) {
  if (value === undefined) return;
  if (typeof value !== 'string') {
    addIssue(issues, field, 'must be a string.');
    return;
  }
  if (!value.trim()) {
    addIssue(issues, field, 'must not be empty.');
    return;
  }
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: value });
  } catch {
    addIssue(issues, field, 'must be a valid IANA timezone, e.g. Europe/Warsaw.');
  }
}

function validateOperatorConfig(config) {
  const issues = [];

  if (!isPlainObject(config)) {
    addIssue(issues, 'config', 'must be a JSON object.');
    return { valid: false, issues };
  }

  validateBoolean(config.enabled, 'enabled', issues);
  validateBoolean(config.startupVerboseLogs, 'startupVerboseLogs', issues);
  validateString(config.cronSchedule, 'cronSchedule', issues, { allowEmpty: false });
  if (typeof config.cronSchedule === 'string' && config.cronSchedule.trim()) {
    const cronValidation = validateCronExpression(config.cronSchedule);
    if (!cronValidation.valid) {
      addIssue(issues, 'cronSchedule', `must be a valid cron expression (${cronValidation.error?.message || 'invalid cron'}).`);
    }
  }
  validateTimeZone(config.timezone, 'timezone', issues);
  validateInteger(config.deleteMessagesOlderThanDays, 'deleteMessagesOlderThanDays', issues, { min: 0 });
  validateInteger(config.deleteLimit, 'deleteLimit', issues, { min: 0 });
  validateInteger(config.deleteIntervalMinutes, 'deleteIntervalMinutes', issues, { min: 0 });
  validateInteger(config.deleteIntervalHours, 'deleteIntervalHours', issues, { min: 0 });
  validateInteger(config.deleteIntervalDays, 'deleteIntervalDays', issues, { min: 0 });
  validateBoolean(config.deletePinned, 'deletePinned', issues);
  validateBoolean(config.dryRun, 'dryRun', issues);
  validateIdString(config.logChannelId, 'logChannelId', issues, { allowEmpty: true });
  validateStringArray(config.ownerIds, 'ownerIds', issues);

  if (config.notifyBeforeDelete !== undefined) {
    if (!isPlainObject(config.notifyBeforeDelete)) {
      addIssue(issues, 'notifyBeforeDelete', 'must be an object.');
    } else {
      validateBoolean(config.notifyBeforeDelete.enabled, 'notifyBeforeDelete.enabled', issues);
      validateInteger(config.notifyBeforeDelete.minutesBefore, 'notifyBeforeDelete.minutesBefore', issues, { min: 0 });
      validateString(config.notifyBeforeDelete.message, 'notifyBeforeDelete.message', issues, { allowEmpty: false });
    }
  }

  if (config.whitelist !== undefined) {
    if (!isPlainObject(config.whitelist)) {
      addIssue(issues, 'whitelist', 'must be an object.');
    } else {
      validateStringArray(config.whitelist.users, 'whitelist.users', issues);
      validateStringArray(config.whitelist.roles, 'whitelist.roles', issues);
    }
  }

  if (config.channels !== undefined) {
    if (!Array.isArray(config.channels)) {
      addIssue(issues, 'channels', 'must be an array.');
    } else {
      config.channels.forEach((channel, index) => {
        const field = `channels[${index}]`;
        if (!isPlainObject(channel)) {
          addIssue(issues, field, 'must be an object.');
          return;
        }
        validateIdString(channel.id, `${field}.id`, issues);
        validateBoolean(channel.enabled, `${field}.enabled`, issues);
        validateInteger(channel.deleteLimit, `${field}.deleteLimit`, issues, { min: 0 });
        validateInteger(channel.deleteMessagesOlderThanDays, `${field}.deleteMessagesOlderThanDays`, issues, { min: 0 });
        validateBoolean(channel.deletePinned, `${field}.deletePinned`, issues);
        validateIdString(channel.guildId, `${field}.guildId`, issues, { allowEmpty: true });
      });
    }
  }

  if (config.automod !== undefined) {
    const automod = config.automod;
    if (!isPlainObject(automod)) {
      addIssue(issues, 'automod', 'must be an object.');
    } else {
      validateBoolean(automod.enabled, 'automod.enabled', issues);
      validateIdString(automod.logChannelId, 'automod.logChannelId', issues, { allowEmpty: true });
      validateStringArray(automod.exemptUsers, 'automod.exemptUsers', issues);
      validateStringArray(automod.exemptRoles, 'automod.exemptRoles', issues);

      if (automod.rules !== undefined) {
        if (!isPlainObject(automod.rules)) {
          addIssue(issues, 'automod.rules', 'must be an object.');
        } else {
          validateStringArray(automod.rules.blockedWords, 'automod.rules.blockedWords', issues, { allowEmptyStrings: false });
          validateStringArray(automod.rules.blockedRegex, 'automod.rules.blockedRegex', issues, { allowEmptyStrings: false });
          if (Array.isArray(automod.rules.blockedRegex)) {
            automod.rules.blockedRegex.forEach((pattern, index) => {
              if (typeof pattern !== 'string' || !pattern.trim()) return;
              try {
                new RegExp(pattern);
              } catch {
                addIssue(issues, `automod.rules.blockedRegex[${index}]`, 'must be a valid regular expression.');
              }
            });
          }
          validateStringArray(automod.rules.allowedDomains, 'automod.rules.allowedDomains', issues, { allowEmptyStrings: false });
          validateBoolean(automod.rules.blockInvites, 'automod.rules.blockInvites', issues);
          validateBoolean(automod.rules.blockLinks, 'automod.rules.blockLinks', issues);
          validateInteger(automod.rules.maxMentions, 'automod.rules.maxMentions', issues, { min: 0 });

          if (automod.rules.caps !== undefined) {
            if (!isPlainObject(automod.rules.caps)) {
              addIssue(issues, 'automod.rules.caps', 'must be an object.');
            } else {
              validateBoolean(automod.rules.caps.enabled, 'automod.rules.caps.enabled', issues);
              validateInteger(automod.rules.caps.minLength, 'automod.rules.caps.minLength', issues, { min: 0 });
              validateNumber(automod.rules.caps.ratio, 'automod.rules.caps.ratio', issues, { min: 0, max: 1 });
            }
          }

          if (automod.rules.duplicate !== undefined) {
            if (!isPlainObject(automod.rules.duplicate)) {
              addIssue(issues, 'automod.rules.duplicate', 'must be an object.');
            } else {
              validateBoolean(automod.rules.duplicate.enabled, 'automod.rules.duplicate.enabled', issues);
              validateInteger(automod.rules.duplicate.windowSeconds, 'automod.rules.duplicate.windowSeconds', issues, { min: 0 });
              validateInteger(automod.rules.duplicate.maxRepeats, 'automod.rules.duplicate.maxRepeats', issues, { min: 0 });
            }
          }
        }
      }

      if (automod.actions !== undefined) {
        if (!isPlainObject(automod.actions)) {
          addIssue(issues, 'automod.actions', 'must be an object.');
        } else {
          validateBoolean(automod.actions.delete, 'automod.actions.delete', issues);
          validateInteger(automod.actions.timeoutMinutes, 'automod.actions.timeoutMinutes', issues, { min: 0 });
          validateBoolean(automod.actions.warnInChannel, 'automod.actions.warnInChannel', issues);
          validateString(automod.actions.warnTemplate, 'automod.actions.warnTemplate', issues, { allowEmpty: false });
          validateInteger(automod.actions.warnDeleteAfterSeconds, 'automod.actions.warnDeleteAfterSeconds', issues, { min: 0 });
        }
      }
    }
  }

  return {
    valid: issues.length === 0,
    issues,
  };
}

function assertValidOperatorConfig(config, filePath = 'config.json') {
  const result = validateOperatorConfig(config);
  if (!result.valid) {
    throw new ConfigValidationError(filePath, result.issues);
  }
  return config;
}

function readOperatorConfigSync(configPath, options = {}) {
  const filePath = configPath || path.join(process.cwd(), 'config.json');
  let raw;
  try {
    raw = fs.readFileSync(filePath, 'utf8');
  } catch (error) {
    if (options.allowMissing && error?.code === 'ENOENT') {
      return null;
    }
    throw error;
  }

  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new ConfigParseError(filePath, error);
  }

  return assertValidOperatorConfig(parsed, filePath);
}

function writeOperatorConfigSync(configPath, payload, options = {}) {
  const filePath = configPath || path.join(process.cwd(), 'config.json');
  const fsModule = options.fsModule || fs;
  const dirPath = path.dirname(filePath);
  const tmpPath = path.join(
    dirPath,
    `.${path.basename(filePath)}.${process.pid}.${Date.now()}.tmp`,
  );

  assertValidOperatorConfig(payload, filePath);

  try {
    fsModule.mkdirSync(dirPath, { recursive: true });
    const serialized = `${JSON.stringify(payload, null, 2)}\n`;
    fsModule.writeFileSync(tmpPath, serialized, 'utf8');

    const fd = fsModule.openSync(tmpPath, 'r');
    try {
      fsModule.fsyncSync(fd);
    } finally {
      fsModule.closeSync(fd);
    }

    if (typeof options.beforeRenameHook === 'function') {
      options.beforeRenameHook();
    }

    fsModule.renameSync(tmpPath, filePath);
  } catch (error) {
    try {
      if (fsModule.existsSync(tmpPath)) {
        fsModule.unlinkSync(tmpPath);
      }
    } catch {}
    throw error;
  }

  return payload;
}

function formatConfigError(error, { prefix = '' } = {}) {
  const base = error?.message || 'Unknown config error.';
  return prefix ? `${prefix}${base}` : base;
}

module.exports = {
  ConfigParseError,
  ConfigValidationError,
  assertValidOperatorConfig,
  formatConfigError,
  readOperatorConfigSync,
  validateOperatorConfig,
  writeOperatorConfigSync,
};
