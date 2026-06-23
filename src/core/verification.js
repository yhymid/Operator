const VERIFY_CODE_ALPHABET = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';

function addOneCalendarMonthIso(baseDate) {
  const date = new Date(baseDate);
  const year = date.getUTCFullYear();
  const month = date.getUTCMonth();
  const day = date.getUTCDate();
  const hours = date.getUTCHours();
  const minutes = date.getUTCMinutes();
  const seconds = date.getUTCSeconds();
  const ms = date.getUTCMilliseconds();

  let targetYear = year;
  let targetMonth = month + 1;
  if (targetMonth > 11) {
    targetMonth = 0;
    targetYear += 1;
  }

  const lastDayOfTargetMonth = new Date(Date.UTC(targetYear, targetMonth + 1, 0)).getUTCDate();
  const targetDay = Math.min(day, lastDayOfTargetMonth);

  return new Date(Date.UTC(targetYear, targetMonth, targetDay, hours, minutes, seconds, ms)).toISOString();
}

function generateVerificationCode(length = 6) {
  const normalizedLength = Math.max(1, Math.floor(Number(length) || 0));
  let out = '';
  for (let index = 0; index < normalizedLength; index += 1) {
    out += VERIFY_CODE_ALPHABET[Math.floor(Math.random() * VERIFY_CODE_ALPHABET.length)];
  }
  return out;
}

module.exports = {
  addOneCalendarMonthIso,
  generateVerificationCode,
};
